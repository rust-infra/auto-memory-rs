//! The packaged manual: one section-3 page per MCP tool.
//!
//! Upstream 0.24.0 (#610) ships a man page per tool in the package and serves it over MCP:
//! `memory://man` is the index, `memory://man/<page>` is a page, and `bm man <topic>` prints
//! one. The pages exist so an agent can read a tool's contract before first use instead of
//! guessing at its parameters and burning a turn on the error message.
//!
//! A page reference is accepted the way people and models actually write it —
//! `search-notes(3)`, `3/search-notes`, `search_notes`, `man3/search-notes.md` — because a
//! lookup that fails on a hyphen is a lookup nobody uses twice.

/// One manual page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManualPage {
    /// Canonical page name, `search-notes`.
    pub name: &'static str,
    /// One-line summary for the index.
    pub summary: &'static str,
    /// The page body, as Markdown.
    pub body: &'static str,
}

/// Every page, in the order `man(list=True)` lists them.
const PAGES: &[ManualPage] = &[
    ManualPage {
        name: "search-notes",
        summary: "Search the knowledge base",
        body: SEARCH_NOTES,
    },
    ManualPage {
        name: "read-note",
        summary: "Read a note by title, permalink, or memory URL",
        body: READ_NOTE,
    },
    ManualPage {
        name: "write-note",
        summary: "Create a note",
        body: WRITE_NOTE,
    },
    ManualPage {
        name: "edit-note",
        summary: "Append, prepend, or replace part of a note",
        body: EDIT_NOTE,
    },
    ManualPage {
        name: "move-note",
        summary: "Move or rename a note",
        body: MOVE_NOTE,
    },
    ManualPage {
        name: "delete-note",
        summary: "Delete a note",
        body: DELETE_NOTE,
    },
    ManualPage {
        name: "build-context",
        summary: "Traverse the graph from a memory:// URL",
        body: BUILD_CONTEXT,
    },
    ManualPage {
        name: "list-directory",
        summary: "Browse the vault tree",
        body: LIST_DIRECTORY,
    },
    ManualPage {
        name: "recent-activity",
        summary: "What changed recently",
        body: RECENT_ACTIVITY,
    },
    ManualPage {
        name: "cat",
        summary: "Print a note, one section, or a line range",
        body: CAT,
    },
    ManualPage {
        name: "grep",
        summary: "Find lines matching a pattern",
        body: GREP,
    },
    ManualPage {
        name: "ls",
        summary: "List a directory, with projects as mount points",
        body: LS,
    },
    ManualPage {
        name: "find",
        summary: "Find notes by metadata predicates",
        body: FIND,
    },
    ManualPage {
        name: "tail",
        summary: "The last lines of recently updated notes",
        body: TAIL,
    },
    ManualPage {
        name: "man",
        summary: "Print a manual page",
        body: MAN,
    },
];

const SEARCH_NOTES: &str = r#"# search-notes(3)

## SYNOPSIS

    search_notes(query, search_type="text", entity_types=[], categories=[],
                 note_types=[], tags=[], status=None, after_date=None,
                 metadata_filters=None, valid_at=None, valid_overlaps=None,
                 time_kind=None, page=1, page_size=10, compact=False,
                 search_all_projects=False, projects=[], output_format="text")

## DESCRIPTION

Searches the index. `search_type` selects the ranking:

- `text` (default) — full text, ranked by bm25.
- `title`, `permalink` — match one field only.
- `vector`, `semantic` — nearest neighbours by embedding.
- `hybrid` — fused full text and vector.

A query using `AND`, `OR` or `NOT` always runs as full text: the vector half ignores
Boolean operators, so honoring one there would answer a different question.

## FILTERS

- `metadata_filters` — a frontmatter filter object. Values may be scalars (exact match) or
  operator objects: `$eq`, `$ne`, `$gt`, `$gte`, `$lt`, `$lte`, `$in`, `$contains`,
  `$exists`. The bare spellings (`gte`) are accepted too, and several operators may apply
  to one field.
- `valid_at` — observations whose valid-time range covers this date.
- `valid_overlaps` — observations whose range overlaps `[start, end]`.
- `time_kind` — `effective`, `valid`, `occurred`, `due`, or `mentioned`.

## PAGINATION

`page` and `page_size` walk the result set; `has_more` says whether another page exists.
`compact=True` returns identifiers, metadata and pagination without note bodies, so a
caller can decide what to read before paying for it.

## CROSS-PROJECT

`search_all_projects=True` merges every project. `projects=[...]` narrows to a chosen
subset by name or external id; an unknown name is an error. Every row on a merged page
carries `project` and `project_id`.
"#;

const READ_NOTE: &str = r#"# read-note(3)

## SYNOPSIS

    read_note(identifier, output_format="text", include_frontmatter=False,
              include_content=True, start_line=None, end_line=None)

## DESCRIPTION

Reads one note. The identifier may be a permalink, an exact title, a file path, or a
`memory://` URL.

The default text surface returns the raw markdown. `output_format="json"` returns
`{title, permalink, file_path, content, frontmatter, checksum, updated_at}`.

## BOUNDED READS

`start_line` and `end_line` return numbered lines and name the next range to read, which
is how a long note is walked without loading it whole.

`include_content=False` returns only the metadata, checksum and `updated_at` — the cheap
way to refresh a stale `expected_checksum`.

## REVISIONS

`checksum` is the revision a guarded write compares against. Read it here, pass it to
`write_note` or `edit_note` as `expected_checksum`, and a concurrent edit becomes a
`NOTE_REVISION_CONFLICT` instead of a lost update.
"#;

const WRITE_NOTE: &str = r#"# write-note(3)

## SYNOPSIS

    write_note(title, content, directory=None, note_type="note", tags=[],
               metadata=None, overwrite=False, expected_checksum=None,
               output_format="text")

## DESCRIPTION

Creates a note, or replaces one with `overwrite=True`. An existing note without
`overwrite` is a tool error (`isError: true`, `error: NOTE_ALREADY_EXISTS`).

`directory` is relative to the project root; `/` means the root. A path that escapes the
project is refused.

## GUARDED WRITES

With `overwrite=True` and `expected_checksum`, the note is replaced only while it is still
that revision. Otherwise nothing is written and the result is `NOTE_REVISION_CONFLICT`
carrying the current checksum. A checksum for a path no note owns is also a conflict, so a
note deleted since it was read is not recreated.
"#;

const EDIT_NOTE: &str = r#"# edit-note(3)

## SYNOPSIS

    edit_note(identifier, operation, content, section=None, find_text=None,
              expected_replacements=None, replace_subsections=True, metadata=None,
              expected_checksum=None, output_format="text")

## DESCRIPTION

Applies one edit. `operation` is one of `append`, `prepend`, `find_replace`,
`replace_section`, `insert_before_section`, `insert_after_section`.

A missing note is created by `append`/`prepend` — unless `expected_checksum` is given, in
which case a missing note is a conflict rather than an auto-create.

`replace_section` replaces nested subsections by default; pass `replace_subsections=False`
for the shallow behavior. A heading that is not in the note is an error for the section
operations, so a typo does not silently append a duplicate section.
"#;

const MOVE_NOTE: &str = r#"# move-note(3)

## SYNOPSIS

    move_note(identifier, destination_path=None, destination_folder=None,
              is_directory=False, output_format="text")

## DESCRIPTION

Moves or renames a note, or a whole directory with `is_directory=True`. The permalink is
kept unless the project is configured to update it on move.

A failed move is a tool error. A destination outside the project is refused.
"#;

const DELETE_NOTE: &str = r#"# delete-note(3)

## SYNOPSIS

    delete_note(identifier, is_directory=False, output_format="text")

## DESCRIPTION

Deletes a note, or every note under a directory with `is_directory=True`. The directory
itself is left in place.

A note that does not exist answers `false` rather than an error; a failed delete is a tool
error.
"#;

const BUILD_CONTEXT: &str = r#"# build-context(3)

## SYNOPSIS

    build_context(url, depth=1, timeframe="7d", page=1, page_size=10,
                  max_related=10, compact=False, output_format="json")

## DESCRIPTION

Traverses the graph from a `memory://` URL and returns the primary note with its
observations and related entities.

`depth` is 1–5. `max_related` is at most 100. Both are bounded so one call cannot become a
multi-megabyte vault export; follow the returned `memory://` links for more.

`compact=True` keeps identifiers, relation targets and pagination and drops the bodies. It
also stops loading observations, so the payload carries no `observations` key.
"#;

const LIST_DIRECTORY: &str = r#"# list-directory(3)

## SYNOPSIS

    list_directory(dir_name="/", depth=1, file_name_glob=None,
                   sort=None, page=1, page_size=10, output_format="text")

## DESCRIPTION

Lists one directory of the vault. `sort` is one of `title_asc`, `title_desc`,
`updated_asc`, `updated_desc`; directories sort before files and the order is applied
before pagination.

`file_name_glob` filters results without pruning recursion, so `*.md` with `depth=2` finds
files inside subdirectories.
"#;

const RECENT_ACTIVITY: &str = r#"# recent-activity(3)

## SYNOPSIS

    recent_activity(type=None, timeframe="7d", page=1, page_size=10,
                    output_format="text")

## DESCRIPTION

Lists recent rows for the project. `type` is `entity`, `relation`, `observation`, or a
list. `page_size` is at most 100.
"#;

const CAT: &str = r#"# cat(3)

## SYNOPSIS

    cat(path, section=None, start_line=None, end_line=None, max_chars=None)

## DESCRIPTION

Prints a note's body. `section` reads one heading's section, `start_line`/`end_line` read a
range, and `max_chars` stops after a budget.

Paths are project-relative. A leading segment that names a project routes there, so
`oracle/notes/simple.md` reaches the `oracle` project's note.

## EXAMPLES

    cat("notes/simple.md")
    cat("notes/simple.md", section="Open items")
    cat("notes/simple.md", start_line=120, end_line=180)
"#;

const GREP: &str = r#"# grep(3)

## SYNOPSIS

    grep(pattern, path=None, context_lines=0, max_matches=100, semantic=False)

## DESCRIPTION

Finds lines containing `pattern`, case-insensitively. `context_lines` widens each match
into a window, and overlapping windows merge so a line is never repeated.

Matching is literal by default: a pattern in no note returns no results. Pass
`semantic=True` to rank by meaning instead, which always returns something.
"#;

const LS: &str = r#"# ls(3)

## SYNOPSIS

    ls(path="/")

## DESCRIPTION

Lists a directory's immediate children, directories first. `ls("/")` lists the project
mount points, which is how a caller discovers what it can reach.

A path whose first segment names a project routes there, and the paths this returns can be
passed straight back to `cat`, `grep`, or `find`.
"#;

const FIND: &str = r#"# find(3)

## SYNOPSIS

    find(predicate=[], fields=[], path=None)

## DESCRIPTION

Finds notes whose frontmatter satisfies every `key<op>value` predicate. The operators are
`=`, `!=`, `>`, `>=`, `<`, `<=`; two numeric sides compare numerically, so
`confidence>0.6` is not a string comparison that puts `0.10` above `0.9`.

`fields` projects the named frontmatter values into the output.

## EXAMPLES

    find(predicate=["status=active"])
    find(predicate=["status=active", "confidence>0.6"], fields=["owner"])
"#;

const TAIL: &str = r#"# tail(3)

## SYNOPSIS

    tail(path=None, count=10)

## DESCRIPTION

Prints the last `count` lines of the most recently updated notes, newest first, with a
`==> path <==` header per note. A `path` narrows the set the same way `grep`'s does.
"#;

const MAN: &str = r#"# man(3)

## SYNOPSIS

    man(topic=None, list=False)

## DESCRIPTION

Prints a manual page. `man(list=True)` is apropos: it lists every page with its summary.

Page references parse the way they are written: `search-notes(3)`, `3/search-notes`,
`search_notes`, and `man3/search-notes.md` all name the same page.

Read a tool's page before first use; it documents the parameters the server actually
accepts.
"#;

/// Every page.
#[must_use]
pub fn pages() -> &'static [ManualPage] {
    PAGES
}

/// Normalize a page reference to its canonical name.
///
/// Accepts `search-notes(3)`, `3/search-notes`, `search_notes`, `man3/search-notes.md`, and
/// a bare `search-notes`.
#[must_use]
pub fn normalize_topic(topic: &str) -> String {
    let trimmed = topic.trim();
    // Drop a section suffix: `search-notes(3)` → `search-notes`.
    let without_section = trimmed
        .split_once('(')
        .map_or(trimmed, |(name, _)| name)
        .trim();
    // Drop a `3/` or `man3/` prefix and a `.md` suffix.
    let without_prefix = without_section
        .rsplit_once('/')
        .map_or(without_section, |(_, name)| name);
    let without_suffix = without_prefix.strip_suffix(".md").unwrap_or(without_prefix);
    without_suffix.trim().replace('_', "-").to_lowercase()
}

/// Find a page by any accepted spelling.
#[must_use]
pub fn find(topic: &str) -> Option<&'static ManualPage> {
    let wanted = normalize_topic(topic);
    PAGES.iter().find(|page| page.name == wanted)
}

/// The `memory://man` index as Markdown.
#[must_use]
pub fn render_index() -> String {
    let mut out = String::from("# Auto Memory Manual\n\n");
    for page in PAGES {
        out.push_str(&format!("- **{}** — {}\n", page.name, page.summary));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_is_reachable_by_every_spelling() {
        for page in pages() {
            for spelling in [
                page.name.to_owned(),
                format!("{}(3)", page.name),
                format!("3/{}", page.name),
                format!("man3/{}.md", page.name),
                page.name.replace('-', "_"),
            ] {
                assert_eq!(
                    find(&spelling).map(|found| found.name),
                    Some(page.name),
                    "spelling {spelling:?} must reach {}",
                    page.name
                );
            }
        }
    }

    #[test]
    fn an_unknown_topic_returns_none() {
        assert!(find("no-such-tool").is_none());
    }

    #[test]
    fn every_page_has_a_synopsis_and_a_description() {
        for page in pages() {
            assert!(
                page.body.contains("## SYNOPSIS"),
                "{} must document its synopsis",
                page.name
            );
            assert!(
                page.body.contains("## DESCRIPTION"),
                "{} must describe itself",
                page.name
            );
            assert!(!page.summary.is_empty(), "{} needs a summary", page.name);
        }
    }

    #[test]
    fn the_index_lists_every_page() {
        let index = render_index();
        for page in pages() {
            assert!(
                index.contains(page.name),
                "{} missing from the index",
                page.name
            );
        }
    }

    #[test]
    fn the_posix_tools_have_pages() {
        // Upstream 0.24.0 ships a page per tool, so a tool without one is a gap a caller
        // notices the moment it asks for help.
        for tool in ["cat", "grep", "ls", "find", "tail", "man"] {
            assert!(find(tool).is_some(), "{tool} needs a manual page");
        }
    }
}
