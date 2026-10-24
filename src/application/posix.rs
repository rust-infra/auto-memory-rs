//! POSIX-style read tools: `cat`, `grep`, `ls`, `find`, `tail`, and `man`.
//!
//! Upstream 0.24.0 (#1398, #1501, #610) adds a second tool surface beside the rich note
//! tools. The vocabulary is deliberately familiar — an agent that has read a shell manual
//! already knows how to use it — and every path it returns can be passed straight back in.
//!
//! Two properties make the surface usable rather than merely familiar:
//!
//! - **Projects are mount points.** `ls /` lists them, and a path whose first segment names
//!   a project routes there, so `/notes/simple.md` and `/oracle/notes/simple.md` are the
//!   same note in the default project and an explicit one respectively.
//! - **A note is addressable by heading or by line range.** The `note_section` index makes
//!   `cat` able to read one section, a line range, or a token budget without loading the
//!   whole file into the answer.
//!
//! The tools return plain text, not JSON: they exist to be read.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::markdown::sections::{NoteSection, find_section, split_sections};
use crate::storage::Store;

/// One project as the POSIX tools see it: a mount point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountPoint {
    /// Project name, which is also the mount's first path segment.
    pub name: String,
    /// Project permalink.
    pub permalink: String,
    /// Filesystem path of the vault, for diagnostics.
    pub path: String,
}

/// A path resolved against the project mounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath {
    /// Project the path landed in.
    pub project: MountPoint,
    /// Path inside that project, without a leading slash.
    pub relative: String,
}

/// List the projects as mounts, for `ls /`.
pub async fn mounts(store: &Store) -> Result<Vec<MountPoint>> {
    Ok(store
        .projects()
        .await?
        .into_iter()
        .map(|project| MountPoint {
            name: project.name,
            permalink: project.permalink,
            path: project.path,
        })
        .collect())
}

/// Resolve `path` against the mounts.
///
/// A leading segment that names a project routes there; anything else resolves inside
/// `default_project`. `..` is refused rather than clamped, because a path that climbs out
/// of a project is a caller mistake, not an empty directory.
pub async fn resolve_path(
    store: &Store,
    default_project: &str,
    path: &str,
) -> Result<ResolvedPath> {
    let trimmed = path.trim().trim_start_matches('/').trim_end_matches('/');
    if trimmed.split('/').any(|segment| segment == "..") {
        return Err(Error::InvalidArgument {
            message: format!("path '{path}' must stay within a project"),
        });
    }
    let mounts = mounts(store).await?;
    if trimmed.is_empty() {
        let Some(project) = mounts.iter().find(|mount| mount.name == default_project) else {
            return Err(Error::InvalidArgument {
                message: format!("project not found: {default_project}"),
            });
        };
        return Ok(ResolvedPath {
            project: project.clone(),
            relative: String::new(),
        });
    }
    let (first, rest) = trimmed.split_once('/').unwrap_or((trimmed, ""));
    if let Some(project) = mounts.iter().find(|mount| mount.name == first) {
        return Ok(ResolvedPath {
            project: project.clone(),
            relative: rest.to_owned(),
        });
    }
    let Some(project) = mounts.iter().find(|mount| mount.name == default_project) else {
        return Err(Error::InvalidArgument {
            message: format!("project not found: {default_project}"),
        });
    };
    Ok(ResolvedPath {
        project: project.clone(),
        relative: trimmed.to_owned(),
    })
}

/// How much of a note `cat` should return.
#[derive(Debug, Clone, Default)]
pub struct CatOptions {
    /// Read only this heading's section.
    pub section: Option<String>,
    /// First line to return, 1-based.
    pub start_line: Option<usize>,
    /// Last line to return, inclusive.
    pub end_line: Option<usize>,
    /// Stop after roughly this many characters.
    pub max_chars: Option<usize>,
}

/// The result of `cat`: the text plus what was actually returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatResult {
    /// The note's project-relative path.
    pub file_path: String,
    /// The returned text.
    pub text: String,
    /// First line returned, 1-based.
    pub start_line: usize,
    /// Last line returned, inclusive.
    pub end_line: usize,
    /// Total lines in the note.
    pub total_lines: usize,
    /// Whether a range or budget cut the note short.
    pub truncated: bool,
}

/// Read one note's body, optionally by section, line range, or character budget.
///
/// The body is read from disk rather than from the index: `cat` is the tool a caller uses
/// to see the file as it is, and a stale index would make it lie.
pub fn cat(file_path: &str, body: &str, options: &CatOptions) -> Result<CatResult> {
    let lines: Vec<&str> = body.split('\n').collect();
    let total = if body.is_empty() {
        0
    } else if body.ends_with('\n') {
        lines.len() - 1
    } else {
        lines.len()
    };

    // A section read wins over a line range: naming both is a caller mistake the section
    // makes unambiguous.
    let (start, end) = if let Some(wanted) = &options.section {
        let sections = split_sections(body);
        let Some(section) = find_section(&sections, wanted) else {
            return Err(Error::InvalidArgument {
                message: format!(
                    "section '{wanted}' not found in {file_path}. Sections: {}",
                    sections
                        .iter()
                        .filter(|section: &&NoteSection| !section.heading.is_empty())
                        .map(NoteSection::anchor)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        };
        (section.start_line, section.end_line)
    } else {
        let start = options.start_line.unwrap_or(1).max(1);
        if total > 0 && start > total {
            return Err(Error::InvalidArgument {
                message: format!(
                    "start_line {start} is past the end of the document ({total} lines)"
                ),
            });
        }
        let end = options.end_line.unwrap_or(total).min(total).max(start);
        (start, end)
    };

    // A range read is numbered, like `read_note`'s bounded read: a caller that asked for
    // lines 120–180 needs to be able to cite them. A whole-note read stays raw, because
    // that is the text the file holds.
    let range_read =
        options.section.is_some() || options.start_line.is_some() || options.end_line.is_some();
    let slice = &lines[start.saturating_sub(1)..end.min(total).max(start.saturating_sub(1))];
    let mut text = if range_read {
        slice
            .iter()
            .enumerate()
            .map(|(offset, line)| format!("{:>4} | {line}", start + offset))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        slice.join("\n")
    };
    let mut truncated = end < total;
    if let Some(budget) = options.max_chars
        && text.chars().count() > budget
    {
        text = text.chars().take(budget).collect();
        truncated = true;
    }
    Ok(CatResult {
        file_path: file_path.to_owned(),
        text,
        start_line: start,
        end_line: end.min(total).max(start),
        total_lines: total,
        truncated,
    })
}

/// Render a `cat` result the way the tool answers: the text, then a provenance line.
#[must_use]
pub fn render_cat(result: &CatResult) -> String {
    let mut out = result.text.clone();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    let range = if result.start_line == 1 && result.end_line == result.total_lines {
        format!("{} lines", result.total_lines)
    } else {
        format!(
            "lines {}-{} of {}",
            result.start_line, result.end_line, result.total_lines
        )
    };
    out.push_str(&format!(
        "\n<!-- {} | {range}{} -->\n",
        result.file_path,
        if result.truncated { " | truncated" } else { "" }
    ));
    out
}

/// One `grep` match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GrepMatch {
    /// Project-relative path of the note.
    pub file_path: String,
    /// 1-based line number of the match.
    pub line_number: usize,
    /// The matching line.
    pub line: String,
}

/// Find lines containing `pattern`, case-insensitively by default.
///
/// `context_lines` widens each match into a window, and overlapping windows merge so a
/// caller does not see the same line twice. `max_matches` bounds the answer.
#[must_use]
pub fn grep(
    notes: &[(String, String)],
    pattern: &str,
    context_lines: usize,
    max_matches: usize,
) -> Vec<GrepMatch> {
    let needle = pattern.to_lowercase();
    let mut matches = Vec::new();
    for (file_path, body) in notes {
        let lines: Vec<&str> = body.split('\n').collect();
        let mut last_emitted = 0usize;
        for (index, line) in lines.iter().enumerate() {
            if !line.to_lowercase().contains(&needle) {
                continue;
            }
            let line_number = index + 1;
            // Widen by the context, then merge with the previous window so a run of
            // matches reads as one block rather than repeating its neighbours.
            let window_start = line_number.saturating_sub(context_lines).max(1);
            let window_end = (line_number + context_lines).min(lines.len());
            for emit in window_start..=window_end {
                if emit <= last_emitted {
                    continue;
                }
                matches.push(GrepMatch {
                    file_path: file_path.clone(),
                    line_number: emit,
                    line: lines[emit - 1].to_owned(),
                });
                last_emitted = emit;
                if matches.len() >= max_matches {
                    return matches;
                }
            }
        }
    }
    matches
}

/// Render grep matches as `path:line: text`, the shape a shell user expects.
#[must_use]
pub fn render_grep(matches: &[GrepMatch]) -> String {
    if matches.is_empty() {
        return String::new();
    }
    matches
        .iter()
        .map(|found| format!("{}:{}: {}", found.file_path, found.line_number, found.line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One entry in an `ls` listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListEntry {
    /// Base name.
    pub name: String,
    /// Path relative to the project root.
    pub path: String,
    /// Whether this is a directory.
    pub is_directory: bool,
}

/// List one directory's immediate children, directories first then files by name.
#[must_use]
pub fn ls(notes: &[(String, String)], directory: &str) -> Vec<ListEntry> {
    let prefix = if directory.is_empty() {
        String::new()
    } else {
        format!("{}/", directory.trim_matches('/'))
    };
    let mut entries: Vec<ListEntry> = Vec::new();
    let mut seen_directories: Vec<String> = Vec::new();
    for (file_path, _) in notes {
        let Some(rest) = file_path.strip_prefix(&prefix) else {
            continue;
        };
        match rest.split_once('/') {
            Some((directory_name, _)) => {
                if !seen_directories.iter().any(|seen| seen == directory_name) {
                    seen_directories.push(directory_name.to_owned());
                    entries.push(ListEntry {
                        name: directory_name.to_owned(),
                        path: format!("{prefix}{directory_name}"),
                        is_directory: true,
                    });
                }
            }
            None => entries.push(ListEntry {
                name: rest.to_owned(),
                path: file_path.clone(),
                is_directory: false,
            }),
        }
    }
    entries.sort_by(|left, right| {
        right
            .is_directory
            .cmp(&left.is_directory)
            .then_with(|| left.name.cmp(&right.name))
    });
    entries
}

/// One `find` hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FindHit {
    /// Project-relative path.
    pub path: String,
    /// The note's title.
    pub title: String,
    /// Frontmatter values the caller asked to project.
    pub fields: Vec<(String, String)>,
}

/// Find notes whose metadata satisfies every `key<op>value` predicate.
///
/// The operators mirror the metadata filter surface (`=`, `!=`, `>`, `>=`, `<`, `<=`), so
/// `find status=active confidence>0.6` reads the way a shell user expects.
#[must_use]
pub fn find(
    notes: &[FindNote<'_>],
    predicates: &[MetadataPredicate],
    fields: &[String],
) -> Vec<FindHit> {
    notes
        .iter()
        .filter(|note| {
            predicates
                .iter()
                .all(|predicate| predicate.matches(&note.metadata))
        })
        .map(|note| FindHit {
            path: note.file_path.to_owned(),
            title: note.title.to_owned(),
            fields: fields
                .iter()
                .filter_map(|field| {
                    note.metadata
                        .get(field)
                        .map(|value| (field.clone(), value.clone()))
                })
                .collect(),
        })
        .collect()
}

/// One note as `find` sees it: its path, title, and stringified frontmatter.
#[derive(Debug, Clone)]
pub struct FindNote<'a> {
    /// Project-relative path.
    pub file_path: &'a str,
    /// Note title.
    pub title: &'a str,
    /// Frontmatter as `key -> rendered value`.
    pub metadata: std::collections::BTreeMap<String, String>,
}

/// One `key<op>value` predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataPredicate {
    /// Frontmatter key.
    pub key: String,
    /// Comparison operator.
    pub op: PredicateOp,
    /// Right-hand side, as written.
    pub value: String,
}

/// The comparison a [`MetadataPredicate`] applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredicateOp {
    /// `=`
    Eq,
    /// `!=`
    Ne,
    /// `>`
    Gt,
    /// `>=`
    Gte,
    /// `<`
    Lt,
    /// `<=`
    Lte,
}

impl MetadataPredicate {
    /// Parse one `key<op>value` expression.
    ///
    /// The two-character operators are tried first so `>=` is not read as `>` followed by
    /// a value of `=x`.
    pub fn parse(expression: &str) -> Result<Self> {
        for (symbol, op) in [
            (">=", PredicateOp::Gte),
            ("<=", PredicateOp::Lte),
            ("!=", PredicateOp::Ne),
            ("=", PredicateOp::Eq),
            (">", PredicateOp::Gt),
            ("<", PredicateOp::Lt),
        ] {
            if let Some((key, value)) = expression.split_once(symbol) {
                let key = key.trim();
                if key.is_empty() {
                    break;
                }
                return Ok(Self {
                    key: key.to_owned(),
                    op,
                    value: value.trim().to_owned(),
                });
            }
        }
        Err(Error::InvalidArgument {
            message: format!(
                "find predicate '{expression}' must be key<op>value, where <op> is one of \
                 =, !=, >, >=, <, <="
            ),
        })
    }

    /// Whether `metadata` satisfies this predicate.
    #[must_use]
    pub fn matches(&self, metadata: &std::collections::BTreeMap<String, String>) -> bool {
        let Some(actual) = metadata.get(&self.key) else {
            // A missing key satisfies no predicate, including `!=`: the note never claimed
            // the value, so it is not a candidate.
            return false;
        };
        if let (Ok(left), Ok(right)) = (actual.parse::<f64>(), self.value.parse::<f64>()) {
            // Numeric comparison when both sides parse, so `confidence>0.6` is not a
            // string comparison that puts "0.10" above "0.9".
            return match self.op {
                PredicateOp::Eq => (left - right).abs() < f64::EPSILON,
                PredicateOp::Ne => (left - right).abs() >= f64::EPSILON,
                PredicateOp::Gt => left > right,
                PredicateOp::Gte => left >= right,
                PredicateOp::Lt => left < right,
                PredicateOp::Lte => left <= right,
            };
        }
        match self.op {
            PredicateOp::Eq => actual == &self.value,
            PredicateOp::Ne => actual != &self.value,
            // Ordering on a non-numeric value is a string comparison, which is still what a
            // caller means for an ISO date.
            PredicateOp::Gt => actual > &self.value,
            PredicateOp::Gte => actual >= &self.value,
            PredicateOp::Lt => actual < &self.value,
            PredicateOp::Lte => actual <= &self.value,
        }
    }
}

/// Render `find` hits one per line: `path  title  [key=value ...]`.
#[must_use]
pub fn render_find(hits: &[FindHit]) -> String {
    hits.iter()
        .map(|hit| {
            let fields: Vec<String> = hit
                .fields
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect();
            if fields.is_empty() {
                format!("{}  {}", hit.path, hit.title)
            } else {
                format!("{}  {}  [{}]", hit.path, hit.title, fields.join(" "))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The last `count` lines of a note, most recently modified first when several match.
#[must_use]
pub fn tail(body: &str, count: usize) -> Vec<(usize, String)> {
    let lines: Vec<&str> = body.split('\n').collect();
    let total = if body.is_empty() {
        0
    } else if body.ends_with('\n') {
        lines.len() - 1
    } else {
        lines.len()
    };
    let start = total.saturating_sub(count);
    (start..total)
        .map(|index| (index + 1, lines[index].to_owned()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "# Title\n\nIntro.\n\n## Open items\n\n- one\n- two\n\n## Closed\n\ndone\n";

    fn notes() -> Vec<(String, String)> {
        vec![
            ("notes/alpha.md".to_owned(), NOTE.to_owned()),
            (
                "notes/nested/beta.md".to_owned(),
                "# Beta\n\nbody\n".to_owned(),
            ),
            ("root.md".to_owned(), "# Root\n".to_owned()),
        ]
    }

    #[test]
    fn cat_returns_the_whole_note_by_default() {
        let result = cat("notes/alpha.md", NOTE, &CatOptions::default()).expect("cat");
        assert_eq!(result.start_line, 1);
        assert_eq!(result.end_line, result.total_lines);
        assert!(!result.truncated, "a whole-note read is not truncated");
        assert!(
            result.text.contains("## Closed"),
            "the whole body must be returned: {}",
            result.text
        );
    }

    #[test]
    fn cat_reads_one_section_by_heading() {
        let options = CatOptions {
            section: Some("Open items".to_owned()),
            ..CatOptions::default()
        };
        let result = cat("notes/alpha.md", NOTE, &options).expect("cat");
        assert!(
            result.text.contains("## Open items"),
            "the heading must be in the section: {}",
            result.text
        );
        assert!(
            result.text.contains("- two"),
            "the section body must be returned: {}",
            result.text
        );
        // The next `##` (line 10) ends the section's own range, so its last own line is 9.
        assert_eq!(result.end_line, 9);
    }

    #[test]
    fn cat_refuses_an_unknown_section_and_lists_the_real_ones() {
        let options = CatOptions {
            section: Some("Missing".to_owned()),
            ..CatOptions::default()
        };
        let error = cat("notes/alpha.md", NOTE, &options).expect_err("missing section");
        let message = error.to_string();
        assert!(message.contains("section 'Missing' not found"), "{message}");
        assert!(message.contains("Open items"), "{message}");
    }

    #[test]
    fn cat_reads_a_line_range() {
        let options = CatOptions {
            start_line: Some(5),
            end_line: Some(7),
            ..CatOptions::default()
        };
        let result = cat("notes/alpha.md", NOTE, &options).expect("cat");
        assert_eq!((result.start_line, result.end_line), (5, 7));
        assert!(result.truncated, "a line range cuts the note short");
        // A range read is numbered, so the caller can cite the lines it read.
        assert_eq!(result.text, "   5 | ## Open items\n   6 | \n   7 | - one");
    }

    #[test]
    fn cat_refuses_a_start_past_the_end() {
        let options = CatOptions {
            start_line: Some(500),
            ..CatOptions::default()
        };
        let error = cat("notes/alpha.md", NOTE, &options).expect_err("past end");
        assert!(error.to_string().contains("past the end"), "{error}");
    }

    #[test]
    fn cat_honours_a_character_budget() {
        let options = CatOptions {
            max_chars: Some(10),
            ..CatOptions::default()
        };
        let result = cat("notes/alpha.md", NOTE, &options).expect("cat");
        assert_eq!(result.text.chars().count(), 10);
        assert!(result.truncated, "a character budget cuts the note short");
    }

    #[test]
    fn grep_finds_lines_case_insensitively() {
        let found = grep(&notes(), "BODY", 0, 100);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].file_path, "notes/nested/beta.md");
        assert_eq!(found[0].line_number, 3);
    }

    #[test]
    fn grep_context_windows_merge_instead_of_repeating() {
        // `- one` and `- two` are adjacent, so their windows overlap and each line appears
        // once.
        let found = grep(&notes(), "- ", 2, 100);
        let numbers: Vec<usize> = found
            .iter()
            .filter(|found| found.file_path == "notes/alpha.md")
            .map(|found| found.line_number)
            .collect();
        let mut unique = numbers.clone();
        unique.dedup();
        assert_eq!(numbers, unique, "a line must not repeat: {numbers:?}");
        assert!(numbers.contains(&7), "{numbers:?}");
        assert!(numbers.contains(&8), "{numbers:?}");
    }

    #[test]
    fn grep_honours_max_matches() {
        let found = grep(&notes(), "e", 0, 3);
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn grep_with_no_match_returns_nothing() {
        assert!(grep(&notes(), "zzzz-not-there", 0, 100).is_empty());
    }

    #[test]
    fn ls_lists_directories_first_then_files() {
        let entries = ls(&notes(), "");
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        // `notes` is the only directory, so it sorts ahead of the files.
        assert_eq!(names[0], "notes");
        assert!(
            entries[0].is_directory,
            "a directory sorts before the files"
        );
        assert!(names.contains(&"root.md"), "{names:?}");
    }

    #[test]
    fn ls_of_a_subdirectory_lists_its_children() {
        let entries = ls(&notes(), "notes");
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert!(names.contains(&"alpha.md"), "{names:?}");
        assert!(names.contains(&"nested"), "{names:?}");
        // A file outside the directory is not listed.
        assert!(!names.contains(&"root.md"), "{names:?}");
    }

    #[test]
    fn find_matches_numeric_predicates_numerically() {
        let mut metadata = std::collections::BTreeMap::new();
        metadata.insert("confidence".to_owned(), "0.9".to_owned());
        let note = FindNote {
            file_path: "a.md",
            title: "A",
            metadata,
        };
        let predicate = MetadataPredicate::parse("confidence>0.6").expect("predicate");
        assert!(
            predicate.matches(&note.metadata),
            "0.9 is above 0.6 numerically"
        );
        // The string comparison would have put "0.9" below "0.6"; the numeric one does not.
        let low = MetadataPredicate::parse("confidence>0.95").expect("predicate");
        assert!(
            !low.matches(&note.metadata),
            "0.9 is below 0.95, so the predicate must not match"
        );
    }

    #[test]
    fn find_projects_the_requested_fields() {
        let mut metadata = std::collections::BTreeMap::new();
        metadata.insert("status".to_owned(), "active".to_owned());
        metadata.insert("owner".to_owned(), "sam".to_owned());
        let note = FindNote {
            file_path: "a.md",
            title: "A",
            metadata,
        };
        let hits = find(
            &[note],
            &[MetadataPredicate::parse("status=active").expect("predicate")],
            &["owner".to_owned()],
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].fields, vec![("owner".to_owned(), "sam".to_owned())]);
    }

    #[test]
    fn find_requires_every_predicate() {
        let mut metadata = std::collections::BTreeMap::new();
        metadata.insert("status".to_owned(), "active".to_owned());
        let note = FindNote {
            file_path: "a.md",
            title: "A",
            metadata,
        };
        let hits = find(
            &[note],
            &[
                MetadataPredicate::parse("status=active").expect("predicate"),
                MetadataPredicate::parse("owner=sam").expect("predicate"),
            ],
            &[],
        );
        assert!(hits.is_empty(), "an unmet predicate must exclude the note");
    }

    #[test]
    fn a_malformed_predicate_is_refused() {
        let error = MetadataPredicate::parse("status").expect_err("no operator");
        assert!(
            error.to_string().contains("must be key<op>value"),
            "{error}"
        );
    }

    #[test]
    fn the_two_character_operators_win_over_the_one_character_ones() {
        let predicate = MetadataPredicate::parse("confidence>=0.6").expect("predicate");
        assert_eq!(predicate.op, PredicateOp::Gte);
        assert_eq!(predicate.value, "0.6");
    }

    #[test]
    fn tail_returns_the_last_lines() {
        let lines = tail(NOTE, 3);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines.last().expect("last").1, "done");
        // `NOTE` has 12 lines; the trailing newline is not a thirteenth.
        assert_eq!(lines.last().expect("last").0, 12);
    }

    #[test]
    fn tail_of_a_short_note_returns_everything() {
        assert_eq!(tail("# A\n", 100).len(), 1);
    }
}
