//! Typed arguments for every MCP tool.
//!
//! `tools/call` hands the server one untyped JSON object per call. Each tool's
//! arguments are mapped onto a struct here, so a handler reads named, typed fields
//! instead of indexing a `Value`, and a tool's argument surface is one place to look
//! at rather than the read sites scattered through its implementation.
//!
//! Three rules keep the mapping compatible with what clients already send. None of
//! them is new behaviour: they reproduce the reference's `dict` reads, which is what
//! `tests/mcp_golden.rs` and `tests/mcp_http.rs` pin.
//!
//! 1. **Every field is an `Option`.** Absence is data, not an error. A tool that
//!    *requires* an argument enforces that where the argument is used, with the
//!    reference's own wording ([`super::helpers::required_str`]), instead of leaning on
//!    deserialization to reject the call.
//! 2. **An argument of the wrong JSON type reads as absent.** The reference read each
//!    key on its own — `arguments["page"].as_u64()` — so `page: "5"` fell back to the
//!    default instead of failing the call. [`lax`] restores that per field: a bare
//!    `Option<u64>` would reject the field and, with it, the whole object. A payload
//!    that is not an object at all (a string, a list) is likewise not an error —
//!    [`ToolArguments::from_arguments`] yields the all-absent default, which is what
//!    indexing a non-object `Value` used to produce.
//! 3. **Aliases stay separate fields.** `page`/`page_number`,
//!    `page_size`/`limit`/`per_page` and the other historical spellings are read in a
//!    documented precedence order by the tool that owns them. `#[serde(alias)]` would
//!    be shorter, but it rejects a call that sends two spellings at once — a behaviour
//!    change for a call that works today.
//!
//! Keys this port does not model are ignored, exactly as before, so a client that also
//! sends `project`, `project_id`, `folder` or some newer key still works.

use serde::Deserialize;
use serde_json::{Map, Value};

use super::helpers::strings;
use super::server::OutputFormat;
use crate::application::directory::DEFAULT_DIRECTORY_PAGE_SIZE;

/// `null`, standing in for an argument the caller left out.
///
/// The shared helpers take `&Value` and spell "absent" as `Value::Null`
/// (`helpers::metadata_pairs`, `helpers::parse_activity_types`), so the accessors that
/// serve them hand out a reference to this rather than cloning a null per call.
static ABSENT: Value = Value::Null;

/// Read one field the way the reference did: whatever JSON type turns up, a value that
/// does not fit `T` counts as "not supplied" instead of as a parse error.
///
/// Deserializing through `Value` first is what makes that a per-field decision. A
/// struct of plain `Option<T>` fields would instead fail the whole object on the first
/// mistyped field — turning `{"title": "x", "content": 5}` into "title is required".
fn lax<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

/// [`lax`] for the arguments the reference read with `helpers::strings`, i.e. "every
/// string in the list", where anything that is not a list means no filter at all.
fn lax_strings<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(value.is_array().then(|| strings(&value)))
}

/// A `tools/call` object, read into one tool's arguments.
///
/// The blanket implementation is what lets `call_tool` name the arguments type at its
/// dispatch arm — `self.read_note(&ReadNoteParams::from_arguments(&arguments))` —
/// without twenty hand-written `impl` blocks.
pub(crate) trait ToolArguments: Default + serde::de::DeserializeOwned {
    /// Map one `tools/call` object onto this tool's arguments.
    ///
    /// Anything that cannot be read as an object at all falls back to the all-absent
    /// default, which is what indexing it as one used to do.
    fn from_arguments(arguments: &Value) -> Self {
        serde_json::from_value(arguments.clone()).unwrap_or_default()
    }
}

impl<T: Default + serde::de::DeserializeOwned> ToolArguments for T {}

/// `write_note`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WriteNoteParams {
    /// Required: the note title.
    #[serde(deserialize_with = "lax")]
    pub(crate) title: Option<String>,
    /// The note body; the reference treats a missing body as empty.
    #[serde(deserialize_with = "lax")]
    pub(crate) content: Option<String>,
    /// Target directory inside the project; `"/"` means the project root.
    #[serde(deserialize_with = "lax")]
    pub(crate) directory: Option<String>,
    /// Whether an existing note may be clobbered.
    #[serde(deserialize_with = "lax")]
    pub(crate) overwrite: Option<bool>,
    /// Frontmatter `type` for the new note, unless the content declares one itself.
    #[serde(deserialize_with = "lax")]
    pub(crate) note_type: Option<String>,
    /// Frontmatter pairs, as an object.
    #[serde(deserialize_with = "lax")]
    pub(crate) metadata: Option<Value>,
    /// Tags: a string, a list, or a comma-separated string (see `parse_tags`).
    #[serde(deserialize_with = "lax")]
    pub(crate) tags: Option<Value>,
    #[serde(deserialize_with = "lax")]
    pub(crate) output_format: Option<String>,
}

impl WriteNoteParams {
    /// The frontmatter object, or `null` when the caller supplied none — the shape
    /// `helpers::metadata_pairs` reads.
    pub(crate) fn metadata(&self) -> &Value {
        self.metadata.as_ref().unwrap_or(&ABSENT)
    }

    /// The `tags` argument *as given*, for `markdown::frontmatter::parse_tags`, which
    /// takes an optional value and accepts a string or a list.
    pub(crate) fn tags(&self) -> Option<&Value> {
        self.tags.as_ref()
    }

    pub(crate) fn output_format(&self) -> OutputFormat {
        OutputFormat::from_argument(self.output_format.as_deref(), OutputFormat::Text)
    }
}

/// `read_note`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ReadNoteParams {
    /// Required: a title, permalink, or memory URL.
    #[serde(deserialize_with = "lax")]
    pub(crate) identifier: Option<String>,
    /// Whether the JSON payload keeps the frontmatter block on `content`.
    #[serde(deserialize_with = "lax")]
    pub(crate) include_frontmatter: Option<bool>,
    #[serde(deserialize_with = "lax")]
    pub(crate) page: Option<u64>,
    /// Older spelling of `page`.
    #[serde(deserialize_with = "lax")]
    pub(crate) page_number: Option<u64>,
    #[serde(deserialize_with = "lax")]
    pub(crate) page_size: Option<u64>,
    /// Older spelling of `page_size`.
    #[serde(deserialize_with = "lax")]
    pub(crate) limit: Option<u64>,
    /// Older spelling of `page_size`.
    #[serde(deserialize_with = "lax")]
    pub(crate) per_page: Option<u64>,
    #[serde(deserialize_with = "lax")]
    pub(crate) output_format: Option<String>,
}

impl ReadNoteParams {
    /// `page` wins over `page_number`; the default is the first page.
    pub(crate) fn page(&self) -> u32 {
        self.page.or(self.page_number).unwrap_or(1) as u32
    }

    /// `page_size` wins over `limit`, which wins over `per_page`; the reference pages
    /// its miss suggestions ten at a time.
    pub(crate) fn page_size(&self) -> u32 {
        self.page_size
            .or(self.limit)
            .or(self.per_page)
            .unwrap_or(10) as u32
    }

    pub(crate) fn output_format(&self) -> OutputFormat {
        OutputFormat::from_argument(self.output_format.as_deref(), OutputFormat::Text)
    }
}

/// `edit_note`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct EditNoteParams {
    /// Required: the note to edit.
    #[serde(deserialize_with = "lax")]
    pub(crate) identifier: Option<String>,
    /// Required: `append`, `prepend`, `find_replace`, `replace_section`,
    /// `insert_before_section` or `insert_after_section`.
    #[serde(deserialize_with = "lax")]
    pub(crate) operation: Option<String>,
    /// The text to insert, or the replacement body.
    #[serde(deserialize_with = "lax")]
    pub(crate) content: Option<String>,
    /// Section heading for the section operations.
    #[serde(deserialize_with = "lax")]
    pub(crate) section: Option<String>,
    /// Search text for `find_replace`.
    #[serde(deserialize_with = "lax")]
    pub(crate) find_text: Option<String>,
    /// How many matches `find_replace` is expected to rewrite.
    #[serde(deserialize_with = "lax")]
    pub(crate) expected_replacements: Option<u64>,
    /// Whether a section operation also rewrites its subsections.
    #[serde(deserialize_with = "lax")]
    pub(crate) replace_subsections: Option<bool>,
    /// Frontmatter pairs to merge in.
    #[serde(deserialize_with = "lax")]
    pub(crate) metadata: Option<Value>,
    #[serde(deserialize_with = "lax")]
    pub(crate) output_format: Option<String>,
}

impl EditNoteParams {
    /// The frontmatter object, or `null` when the caller supplied none.
    pub(crate) fn metadata(&self) -> &Value {
        self.metadata.as_ref().unwrap_or(&ABSENT)
    }

    pub(crate) fn output_format(&self) -> OutputFormat {
        OutputFormat::from_argument(self.output_format.as_deref(), OutputFormat::Text)
    }
}

/// `move_note`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct MoveNoteParams {
    /// Required: the note — or, with `is_directory`, the directory prefix — to move.
    #[serde(deserialize_with = "lax")]
    pub(crate) identifier: Option<String>,
    /// Whether `identifier` names a whole directory.
    #[serde(deserialize_with = "lax")]
    pub(crate) is_directory: Option<bool>,
    /// The exact target path.
    #[serde(deserialize_with = "lax")]
    pub(crate) destination_path: Option<String>,
    /// A target folder that keeps the note's own filename.
    #[serde(deserialize_with = "lax")]
    pub(crate) destination_folder: Option<String>,
    #[serde(deserialize_with = "lax")]
    pub(crate) output_format: Option<String>,
}

impl MoveNoteParams {
    pub(crate) fn output_format(&self) -> OutputFormat {
        OutputFormat::from_argument(self.output_format.as_deref(), OutputFormat::Text)
    }
}

/// `delete_note`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct DeleteNoteParams {
    /// Required: the note — or, with `is_directory`, the directory prefix — to delete.
    #[serde(deserialize_with = "lax")]
    pub(crate) identifier: Option<String>,
    /// Whether `identifier` names a whole directory.
    #[serde(deserialize_with = "lax")]
    pub(crate) is_directory: Option<bool>,
    #[serde(deserialize_with = "lax")]
    pub(crate) output_format: Option<String>,
}

impl DeleteNoteParams {
    pub(crate) fn output_format(&self) -> OutputFormat {
        OutputFormat::from_argument(self.output_format.as_deref(), OutputFormat::Text)
    }
}

/// `search_notes`, and the request both the ChatGPT `search` adapter and
/// `search_all_projects`' per-project fan-out build from it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SearchNotesParams {
    /// The text query; a request without one is filter-only.
    #[serde(deserialize_with = "lax")]
    pub(crate) query: Option<String>,
    /// `text` (default), `title`, `permalink`, `vector`, `semantic` or `hybrid`.
    #[serde(deserialize_with = "lax")]
    pub(crate) search_type: Option<String>,
    /// Item types to search, when the caller named any.
    #[serde(deserialize_with = "lax_strings")]
    pub(crate) entity_types: Option<Vec<String>>,
    /// Observation categories, which imply observation rows when `entity_types` is
    /// not given.
    #[serde(deserialize_with = "lax_strings")]
    pub(crate) categories: Option<Vec<String>>,
    /// Exact-title filter.
    #[serde(deserialize_with = "lax")]
    pub(crate) title: Option<String>,
    /// Exact-permalink filter.
    #[serde(deserialize_with = "lax")]
    pub(crate) permalink: Option<String>,
    /// Permalink prefix filter.
    #[serde(deserialize_with = "lax")]
    pub(crate) permalink_match: Option<String>,
    /// Frontmatter types to search.
    #[serde(deserialize_with = "lax_strings")]
    pub(crate) note_types: Option<Vec<String>>,
    /// Tags to search.
    #[serde(deserialize_with = "lax_strings")]
    pub(crate) tags: Option<Vec<String>>,
    /// Frontmatter `status` filter.
    #[serde(deserialize_with = "lax")]
    pub(crate) status: Option<String>,
    /// Lower bound on `updated_at`.
    #[serde(deserialize_with = "lax")]
    pub(crate) after_date: Option<String>,
    /// Older spelling of `after_date`.
    #[serde(deserialize_with = "lax")]
    pub(crate) since: Option<String>,
    /// Older spelling of `after_date`.
    #[serde(deserialize_with = "lax")]
    pub(crate) after: Option<String>,
    /// Older spelling of `after_date`.
    #[serde(deserialize_with = "lax")]
    pub(crate) from_date: Option<String>,
    /// Frontmatter key/value filters.
    #[serde(deserialize_with = "lax")]
    pub(crate) metadata_filters: Option<Value>,
    /// Per-query floor for the vector legs.
    #[serde(deserialize_with = "lax")]
    pub(crate) min_similarity: Option<f64>,
    #[serde(deserialize_with = "lax")]
    pub(crate) page: Option<u64>,
    #[serde(deserialize_with = "lax")]
    pub(crate) page_size: Option<u64>,
    /// Search every registered project and merge the pages.
    #[serde(deserialize_with = "lax")]
    pub(crate) search_all_projects: Option<bool>,
    #[serde(deserialize_with = "lax")]
    pub(crate) output_format: Option<String>,
}

impl SearchNotesParams {
    /// The item types to search; an absent (or non-list) value filters nothing.
    pub(crate) fn entity_types(&self) -> &[String] {
        self.entity_types.as_deref().unwrap_or_default()
    }

    /// The category filter; an absent (or non-list) value filters nothing.
    pub(crate) fn categories(&self) -> &[String] {
        self.categories.as_deref().unwrap_or_default()
    }

    /// The frontmatter-type filter; an absent (or non-list) value filters nothing.
    pub(crate) fn note_types(&self) -> &[String] {
        self.note_types.as_deref().unwrap_or_default()
    }

    /// The tag filter; an absent (or non-list) value filters nothing.
    pub(crate) fn tags(&self) -> &[String] {
        self.tags.as_deref().unwrap_or_default()
    }

    /// The first of `after_date`/`since`/`after`/`from_date` the caller supplied.
    pub(crate) fn after_date(&self) -> Option<&str> {
        self.after_date
            .as_deref()
            .or(self.since.as_deref())
            .or(self.after.as_deref())
            .or(self.from_date.as_deref())
    }

    /// The `metadata_filters` object, if the caller sent one: an absent or non-object
    /// value filters nothing.
    pub(crate) fn metadata_filters(&self) -> Option<&Map<String, Value>> {
        self.metadata_filters.as_ref().and_then(Value::as_object)
    }

    pub(crate) fn output_format(&self) -> OutputFormat {
        OutputFormat::from_argument(self.output_format.as_deref(), OutputFormat::Text)
    }
}

/// `search` — the OpenAI-client-only ChatGPT compatibility adapter, whose own
/// argument is just the query.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ChatgptSearchParams {
    /// Required: the query to run through `search_notes`.
    #[serde(deserialize_with = "lax")]
    pub(crate) query: Option<String>,
}

/// `fetch` — the OpenAI-client-only ChatGPT compatibility adapter.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ChatgptFetchParams {
    /// Required: a note id, path, or memory URL.
    #[serde(deserialize_with = "lax")]
    pub(crate) id: Option<String>,
}

/// `build_context`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct BuildContextParams {
    /// Required: the `memory://` URL to build context for.
    #[serde(deserialize_with = "lax")]
    pub(crate) url: Option<String>,
    /// How far back the context looks (default `7d`).
    #[serde(deserialize_with = "lax")]
    pub(crate) timeframe: Option<String>,
    /// How many hops of related entities to follow.
    #[serde(deserialize_with = "lax")]
    pub(crate) depth: Option<u64>,
    /// How many related entities to attach.
    #[serde(deserialize_with = "lax")]
    pub(crate) max_related: Option<u64>,
    #[serde(deserialize_with = "lax")]
    pub(crate) page: Option<u64>,
    #[serde(deserialize_with = "lax")]
    pub(crate) page_size: Option<u64>,
    #[serde(deserialize_with = "lax")]
    pub(crate) output_format: Option<String>,
}

impl BuildContextParams {
    /// `build_context` is the one tool that answers with its JSON payload by default;
    /// `output_format="text"` opts into the markdown artifact.
    pub(crate) fn output_format(&self) -> OutputFormat {
        OutputFormat::from_argument(self.output_format.as_deref(), OutputFormat::Json)
    }
}

/// `schema_validate`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SchemaValidateParams {
    /// Restrict the check to one note type.
    #[serde(deserialize_with = "lax")]
    pub(crate) note_type: Option<String>,
    /// Restrict the check to one note.
    #[serde(deserialize_with = "lax")]
    pub(crate) identifier: Option<String>,
    #[serde(deserialize_with = "lax")]
    pub(crate) output_format: Option<String>,
}

impl SchemaValidateParams {
    pub(crate) fn output_format(&self) -> OutputFormat {
        OutputFormat::from_argument(self.output_format.as_deref(), OutputFormat::Text)
    }
}

/// `schema_infer`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SchemaInferParams {
    /// Required: the note type to analyze.
    #[serde(deserialize_with = "lax")]
    pub(crate) note_type: Option<String>,
    /// Share of notes a field must appear in to be suggested (default `0.25`).
    #[serde(deserialize_with = "lax")]
    pub(crate) threshold: Option<f64>,
    #[serde(deserialize_with = "lax")]
    pub(crate) output_format: Option<String>,
}

impl SchemaInferParams {
    pub(crate) fn output_format(&self) -> OutputFormat {
        OutputFormat::from_argument(self.output_format.as_deref(), OutputFormat::Text)
    }
}

/// `schema_diff`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SchemaDiffParams {
    /// Required: the note type to compare.
    #[serde(deserialize_with = "lax")]
    pub(crate) note_type: Option<String>,
    #[serde(deserialize_with = "lax")]
    pub(crate) output_format: Option<String>,
}

impl SchemaDiffParams {
    pub(crate) fn output_format(&self) -> OutputFormat {
        OutputFormat::from_argument(self.output_format.as_deref(), OutputFormat::Text)
    }
}

/// `list_directory`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ListDirectoryParams {
    /// The directory to browse.
    #[serde(deserialize_with = "lax")]
    pub(crate) dir_name: Option<String>,
    /// Older spelling of `dir_name`.
    #[serde(deserialize_with = "lax")]
    pub(crate) directory: Option<String>,
    /// Older spelling of `dir_name`.
    #[serde(deserialize_with = "lax")]
    pub(crate) folder: Option<String>,
    /// Older spelling of `dir_name`.
    #[serde(deserialize_with = "lax")]
    pub(crate) path: Option<String>,
    /// Older spelling of `dir_name`.
    #[serde(deserialize_with = "lax")]
    pub(crate) dir: Option<String>,
    /// How deep to walk.
    #[serde(deserialize_with = "lax")]
    pub(crate) depth: Option<u64>,
    /// Basename glob, which gates inclusion but never recursion.
    #[serde(deserialize_with = "lax")]
    pub(crate) file_name_glob: Option<String>,
    /// Older spelling of `file_name_glob`.
    #[serde(deserialize_with = "lax")]
    pub(crate) glob: Option<String>,
    /// Older spelling of `file_name_glob`.
    #[serde(deserialize_with = "lax")]
    pub(crate) pattern: Option<String>,
    /// Older spelling of `file_name_glob`.
    #[serde(deserialize_with = "lax")]
    pub(crate) filter: Option<String>,
    /// `title_asc` (default), `title_desc`, `updated_asc` or `updated_desc`.
    #[serde(deserialize_with = "lax")]
    pub(crate) sort: Option<String>,
    #[serde(deserialize_with = "lax")]
    pub(crate) page: Option<u64>,
    #[serde(deserialize_with = "lax")]
    pub(crate) page_size: Option<u64>,
    /// Older spelling of `page_size`.
    #[serde(deserialize_with = "lax")]
    pub(crate) limit: Option<u64>,
    /// Older spelling of `page_size`.
    #[serde(deserialize_with = "lax")]
    pub(crate) per_page: Option<u64>,
    #[serde(deserialize_with = "lax")]
    pub(crate) output_format: Option<String>,
}

impl ListDirectoryParams {
    /// The first of `dir_name`/`directory`/`folder`/`path`/`dir`; the project root when
    /// the caller named none.
    pub(crate) fn dir_name(&self) -> &str {
        self.dir_name
            .as_deref()
            .or(self.directory.as_deref())
            .or(self.folder.as_deref())
            .or(self.path.as_deref())
            .or(self.dir.as_deref())
            .unwrap_or("/")
    }

    /// The first of `file_name_glob`/`glob`/`pattern`/`filter`.
    pub(crate) fn file_name_glob(&self) -> Option<&str> {
        self.file_name_glob
            .as_deref()
            .or(self.glob.as_deref())
            .or(self.pattern.as_deref())
            .or(self.filter.as_deref())
    }

    /// `page_size` wins over `limit`, which wins over `per_page`; the reference lists
    /// `DEFAULT_DIRECTORY_PAGE_SIZE` entries at a time.
    pub(crate) fn page_size(&self) -> u32 {
        self.page_size
            .or(self.limit)
            .or(self.per_page)
            .unwrap_or(DEFAULT_DIRECTORY_PAGE_SIZE as u64) as u32
    }

    pub(crate) fn output_format(&self) -> OutputFormat {
        OutputFormat::from_argument(self.output_format.as_deref(), OutputFormat::Text)
    }
}

/// `read_content`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ReadContentParams {
    /// The file to read; required, under any of its spellings.
    #[serde(deserialize_with = "lax")]
    pub(crate) path: Option<String>,
    /// Older spelling of `path`.
    #[serde(deserialize_with = "lax")]
    pub(crate) file_path: Option<String>,
    /// Older spelling of `path`.
    #[serde(deserialize_with = "lax")]
    pub(crate) filepath: Option<String>,
    /// Older spelling of `path`.
    #[serde(deserialize_with = "lax")]
    pub(crate) file: Option<String>,
}

impl ReadContentParams {
    /// The first of `path`/`file_path`/`filepath`/`file` the caller supplied.
    pub(crate) fn path(&self) -> Option<&str> {
        self.path
            .as_deref()
            .or(self.file_path.as_deref())
            .or(self.filepath.as_deref())
            .or(self.file.as_deref())
    }
}

/// `view_note`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ViewNoteParams {
    /// Required: the note to render.
    #[serde(deserialize_with = "lax")]
    pub(crate) identifier: Option<String>,
}

/// `recent_activity`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct RecentActivityParams {
    #[serde(deserialize_with = "lax")]
    pub(crate) page_size: Option<u64>,
    /// Older spelling of `page_size`.
    #[serde(deserialize_with = "lax")]
    pub(crate) limit: Option<u64>,
    /// Older spelling of `page_size`.
    #[serde(deserialize_with = "lax")]
    pub(crate) per_page: Option<u64>,
    /// `entity`, `relation`, `observation`, or a list of them.
    #[serde(rename = "type", deserialize_with = "lax")]
    pub(crate) type_filter: Option<Value>,
    /// How far back to look.
    #[serde(deserialize_with = "lax")]
    pub(crate) timeframe: Option<String>,
    /// Older spelling of `timeframe`.
    #[serde(deserialize_with = "lax")]
    pub(crate) since: Option<String>,
    /// Older spelling of `timeframe`.
    #[serde(deserialize_with = "lax")]
    pub(crate) time_range: Option<String>,
    /// Older spelling of `timeframe`.
    #[serde(deserialize_with = "lax")]
    pub(crate) lookback: Option<String>,
    #[serde(deserialize_with = "lax")]
    pub(crate) depth: Option<u64>,
    #[serde(deserialize_with = "lax")]
    pub(crate) page: Option<u64>,
    /// Older spelling of `page`.
    #[serde(deserialize_with = "lax")]
    pub(crate) page_number: Option<u64>,
    #[serde(deserialize_with = "lax")]
    pub(crate) output_format: Option<String>,
}

impl RecentActivityParams {
    /// `page_size` wins over `limit`, which wins over `per_page`; the reference lists
    /// ten rows at a time.
    pub(crate) fn page_size(&self) -> u64 {
        self.page_size
            .or(self.limit)
            .or(self.per_page)
            .unwrap_or(10)
    }

    /// `page` wins over `page_number`; the default is the first page.
    pub(crate) fn page(&self) -> u32 {
        self.page.or(self.page_number).unwrap_or(1) as u32
    }

    /// The first of `timeframe`/`since`/`time_range`/`lookback`; `7d` by default.
    pub(crate) fn timeframe(&self) -> &str {
        self.timeframe
            .as_deref()
            .or(self.since.as_deref())
            .or(self.time_range.as_deref())
            .or(self.lookback.as_deref())
            .unwrap_or("7d")
    }

    /// The raw `type` argument, or `null` when the caller supplied none — the shape
    /// `helpers::parse_activity_types` reads.
    pub(crate) fn activity_types(&self) -> &Value {
        self.type_filter.as_ref().unwrap_or(&ABSENT)
    }

    pub(crate) fn output_format(&self) -> OutputFormat {
        OutputFormat::from_argument(self.output_format.as_deref(), OutputFormat::Text)
    }
}

/// `list_memory_projects`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ListMemoryProjectsParams {
    #[serde(deserialize_with = "lax")]
    pub(crate) output_format: Option<String>,
}

impl ListMemoryProjectsParams {
    pub(crate) fn output_format(&self) -> OutputFormat {
        OutputFormat::from_argument(self.output_format.as_deref(), OutputFormat::Text)
    }
}

/// `create_memory_project`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct CreateMemoryProjectParams {
    /// Required: the project name the refusal echoes.
    #[serde(deserialize_with = "lax")]
    pub(crate) project_name: Option<String>,
    /// The path the refusal echoes.
    #[serde(deserialize_with = "lax")]
    pub(crate) project_path: Option<String>,
    #[serde(deserialize_with = "lax")]
    pub(crate) output_format: Option<String>,
}

impl CreateMemoryProjectParams {
    pub(crate) fn output_format(&self) -> OutputFormat {
        OutputFormat::from_argument(self.output_format.as_deref(), OutputFormat::Text)
    }
}

/// `delete_project`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct DeleteProjectParams {
    /// Required: the project name the refusal echoes.
    #[serde(deserialize_with = "lax")]
    pub(crate) project_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::mcp::helpers::{parse_activity_types, required_str};
    use crate::domain::search::SearchItemType;
    use serde_json::json;

    /// The reference read each key on its own (`arguments["page"].as_u64()`), so a value
    /// of the wrong JSON type behaved like an omitted one. Plain `serde` would instead
    /// reject the field, and with the container default behind it the whole object would
    /// come back empty.
    #[test]
    fn a_mistyped_argument_reads_as_an_absent_one() {
        let params = ReadNoteParams::from_arguments(&json!({
            "identifier": "notes/welcome",
            "page": "5",
            "page_size": 3,
        }));
        assert_eq!(params.identifier.as_deref(), Some("notes/welcome"));
        assert_eq!(
            params.page(),
            1,
            "a string where a page belongs is not a page"
        );
        assert_eq!(params.page_size(), 3);
    }

    /// The same rule, one field at a time: a mistyped `content` must not cost the call
    /// its `title`.
    #[test]
    fn one_mistyped_field_does_not_take_the_whole_object_down() {
        let params = WriteNoteParams::from_arguments(&json!({ "title": "x", "content": 5 }));
        assert_eq!(params.title.as_deref(), Some("x"));
        assert_eq!(params.content, None);
        assert_eq!(params.overwrite, None);
    }

    /// Indexing a non-object `Value` yielded a null for every key, so a payload that is
    /// not an object means "nothing was supplied" rather than a protocol error.
    #[test]
    fn a_payload_that_is_not_an_object_is_all_absent() {
        for payload in [json!("notes/welcome"), json!([1, 2]), json!(null)] {
            let params = ReadNoteParams::from_arguments(&payload);
            assert_eq!(params.identifier, None);
            assert_eq!(params.page(), 1);
            assert_eq!(
                required_str(params.identifier.as_deref(), "identifier")
                    .unwrap_err()
                    .to_string(),
                "identifier is required",
            );
        }
    }

    /// `#[serde(alias)]` would report a duplicate field here — for a call that works
    /// today. Every spelling keeps its own field and the accessor picks.
    #[test]
    fn sending_two_spellings_of_one_argument_is_not_an_error() {
        let params = ReadNoteParams::from_arguments(&json!({
            "page": 2,
            "page_number": 7,
            "page_size": 3,
            "limit": 4,
            "per_page": 5,
        }));
        assert_eq!(params.page(), 2, "page wins over page_number");
        assert_eq!(
            params.page_size(),
            3,
            "page_size wins over limit and per_page"
        );
        assert_eq!(
            ReadNoteParams::from_arguments(&json!({ "limit": 4, "per_page": 5 })).page_size(),
            4,
        );
        assert_eq!(
            ReadNoteParams::from_arguments(&json!({ "per_page": 5 })).page_size(),
            5,
        );
    }

    /// The long alias chains resolve in the reference's own order, and a missing one
    /// falls back to the tool's default.
    #[test]
    fn long_alias_chains_keep_their_order() {
        let directory =
            ListDirectoryParams::from_arguments(&json!({ "dir": "d", "path": "p", "folder": "f" }));
        assert_eq!(directory.dir_name(), "f", "folder wins over path and dir");
        assert_eq!(ListDirectoryParams::default().dir_name(), "/");
        assert_eq!(
            ListDirectoryParams::default().page_size(),
            DEFAULT_DIRECTORY_PAGE_SIZE,
        );
        let glob =
            ListDirectoryParams::from_arguments(&json!({ "pattern": "*.md", "filter": "*.txt" }));
        assert_eq!(glob.file_name_glob(), Some("*.md"));
        assert_eq!(
            ReadContentParams::from_arguments(&json!({ "file": "a.md", "filepath": "b.md" }))
                .path(),
            Some("b.md"),
            "filepath wins over file",
        );
        assert_eq!(ReadContentParams::default().path(), None);
        assert_eq!(
            SearchNotesParams::from_arguments(&json!({ "from_date": "1d", "after": "2d" }))
                .after_date(),
            Some("2d"),
            "after wins over from_date",
        );
        assert_eq!(
            RecentActivityParams::from_arguments(&json!({ "lookback": "30d", "time_range": "7d" }))
                .timeframe(),
            "7d",
        );
    }

    /// Lists are "the strings in them", and anything that is not a list filters
    /// nothing — `helpers::strings`' own behaviour.
    #[test]
    fn filter_lists_keep_the_strings_and_ignore_everything_else() {
        let params = SearchNotesParams::from_arguments(&json!({ "tags": ["a", 1, null, "b"] }));
        assert_eq!(params.tags().to_vec(), vec!["a".to_owned(), "b".to_owned()]);
        assert!(
            SearchNotesParams::from_arguments(&json!({ "tags": "a" }))
                .tags()
                .is_empty()
        );
        assert!(SearchNotesParams::default().tags().is_empty());
    }

    /// `metadata_pairs` and `parse_activity_types` take `&Value` and spell "absent" as
    /// `Value::Null`, so the accessors must hand out a null rather than a container.
    #[test]
    fn an_absent_metadata_or_type_is_a_null() {
        assert_eq!(
            WriteNoteParams::from_arguments(&json!({ "title": "x" })).metadata(),
            &Value::Null,
        );
        assert_eq!(
            WriteNoteParams::from_arguments(&json!({ "title": "x", "metadata": { "a": 1 } }))
                .metadata(),
            &json!({ "a": 1 }),
        );
        assert_eq!(
            RecentActivityParams::default().activity_types(),
            &Value::Null
        );
        assert_eq!(
            parse_activity_types(
                RecentActivityParams::from_arguments(&json!({ "type": ["entity", "relation"] }))
                    .activity_types()
            )
            .expect("a list of known types"),
            vec![SearchItemType::Entity, SearchItemType::Relation],
        );
    }

    /// The default is per tool, and an unrecognized — or mistyped — value falls back to
    /// it instead of failing the call.
    #[test]
    fn output_format_defaults_are_per_tool_and_lenient() {
        assert_eq!(
            ReadNoteParams::default().output_format(),
            OutputFormat::Text
        );
        assert_eq!(
            BuildContextParams::default().output_format(),
            OutputFormat::Json,
        );
        assert_eq!(
            ReadNoteParams::from_arguments(&json!({ "output_format": "json" })).output_format(),
            OutputFormat::Json,
        );
        assert_eq!(
            ReadNoteParams::from_arguments(&json!({ "output_format": "yaml" })).output_format(),
            OutputFormat::Text,
        );
        assert_eq!(
            ReadNoteParams::from_arguments(&json!({ "output_format": 1 })).output_format(),
            OutputFormat::Text,
        );
    }

    /// Keys this port does not model are ignored, as they were when each handler read
    /// the object key by key.
    #[test]
    fn unknown_keys_are_ignored() {
        let params = SearchNotesParams::from_arguments(&json!({
            "query": "ranking",
            "project": "oracle",
            "project_id": "some-uuid",
        }));
        assert_eq!(params.query.as_deref(), Some("ranking"));
    }
}
