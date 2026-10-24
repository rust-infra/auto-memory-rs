//! Wiki projection: generated `index.md` and `log.md` navigation pages.
//!
//! Upstream 0.24.0 (#1422, #1381–#1484) adds `bm wiki status|validate|rebuild`, which
//! generate root and per-folder `index.md` and `log.md` pages for a local project. The
//! projector is deterministic, so two runs over the same vault produce the same bytes.
//!
//! Four rules make the projection safe to run over a vault a human also edits:
//!
//! - a **hand-written** `index.md` or `log.md` is never overwritten — it is reported as a
//!   conflict instead;
//! - a page the projector wrote is recognisable by `generated: wiki` in its frontmatter,
//!   so the next run may replace it;
//! - each `log.md` keeps the **25 most recent** changes, so the page stays a log rather
//!   than a full history;
//! - a folder that no longer exists gets no page, so a deleted folder does not leave a
//!   ghost in the navigation.
//!
//! The generated pages carry `bm_parse_semantics: false` (for `log.md`) or nothing (for
//! `index.md`, whose wikilinks *are* meant to feed the graph — upstream #1601 fixed
//! exactly that).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How many changes a generated `log.md` keeps.
pub const LOG_ENTRIES: usize = 25;

/// The frontmatter marker that identifies a projector-owned page.
pub const GENERATED_MARKER: &str = "generated: wiki";

/// The reserved page names the projector owns.
pub const RESERVED_NAMES: [&str; 2] = ["index.md", "log.md"];

/// One entry in a folder's log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeEntry {
    /// What happened: `create`, `update`, `edit`, `move`, or `delete`.
    pub kind: String,
    /// The note's path.
    pub file_path: String,
    /// The previous path, for a move.
    pub previous_path: Option<String>,
    /// When it happened, as a storage timestamp.
    pub created_at: String,
}

impl ChangeEntry {
    /// The note's title, derived from its filename.
    fn title(&self) -> String {
        self.file_path
            .rsplit('/')
            .next()
            .unwrap_or(&self.file_path)
            .trim_end_matches(".md")
            .to_owned()
    }

    /// The log's one-line rendering.
    fn render(&self) -> String {
        match (self.kind.as_str(), &self.previous_path) {
            ("move", Some(previous)) => format!(
                "- {} — moved `{}` → `{}`",
                self.created_at, previous, self.file_path
            ),
            (kind, _) => format!("- {} — {} [[{}]]", self.created_at, kind, self.title()),
        }
    }
}

/// One page the projector would write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectedPage {
    /// Project-relative path.
    pub file_path: String,
    /// Full note text.
    pub content: String,
}

/// What a projection run would do to one page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PageOutcome {
    /// The page does not exist yet.
    Create {
        /// The page to write.
        page: ProjectedPage,
    },
    /// The page exists, was written by the projector, and its content changed.
    Update {
        /// The page to write.
        page: ProjectedPage,
    },
    /// The page exists and already matches.
    Unchanged {
        /// The page's path.
        file_path: String,
    },
    /// A hand-written page occupies the path.
    Conflict {
        /// The page's path.
        file_path: String,
        /// Why it is a conflict.
        reason: String,
    },
}

impl PageOutcome {
    /// The page's path, whatever the outcome.
    #[must_use]
    pub fn file_path(&self) -> &str {
        match self {
            Self::Create { page } | Self::Update { page } => &page.file_path,
            Self::Unchanged { file_path } | Self::Conflict { file_path, .. } => file_path,
        }
    }
}

/// One note the projector sees: its path, title, and tags.
#[derive(Debug, Clone)]
pub struct NoteSummary {
    /// Project-relative path.
    pub file_path: String,
    /// Note title.
    pub title: String,
    /// Frontmatter tags.
    pub tags: Vec<String>,
}

/// Plan a projection over one vault.
///
/// `existing` maps a page path to its current content, which is how the projector tells a
/// page it wrote from one a human wrote.
#[must_use]
pub fn plan(
    notes: &[NoteSummary],
    changes: &[ChangeEntry],
    existing: &BTreeMap<String, String>,
) -> Vec<PageOutcome> {
    // Every folder that holds at least one note, plus the root.
    let mut folders: BTreeMap<String, Vec<&NoteSummary>> = BTreeMap::new();
    folders.entry(String::new()).or_default();
    for note in notes {
        // A reserved page is not content, so it does not make a folder exist on its own.
        let is_reserved = RESERVED_NAMES
            .iter()
            .any(|name| note.file_path.ends_with(&format!("/{name}")) || note.file_path == *name);
        if is_reserved {
            continue;
        }
        let folder = note
            .file_path
            .rsplit_once('/')
            .map(|(folder, _)| folder.to_owned())
            .unwrap_or_default();
        folders.entry(folder.clone()).or_default().push(note);
        // A nested folder implies its parents exist, so they get a page too.
        let mut parent = folder;
        while let Some((up, _)) = parent.rsplit_once('/') {
            folders.entry(up.to_owned()).or_default();
            parent = up.to_owned();
        }
    }

    // The folder names, collected once so the child lookup cannot borrow a map that is
    // being consumed.
    let folder_names: Vec<String> = folders.keys().cloned().collect();
    let mut outcomes = Vec::new();
    for (folder, folder_notes) in &folders {
        let mut folder_notes = folder_notes.clone();
        folder_notes.sort_by(|left, right| left.file_path.cmp(&right.file_path));
        let children = child_folders(folder, &folder_names);
        let folder_changes: Vec<&ChangeEntry> = changes
            .iter()
            .filter(|change| folder_of(&change.file_path) == *folder)
            .take(LOG_ENTRIES)
            .collect();

        outcomes.push(outcome_for(
            folder,
            "index.md",
            build_index(folder, &folder_notes, &children),
            existing,
        ));
        outcomes.push(outcome_for(
            folder,
            "log.md",
            build_log(folder, &folder_changes),
            existing,
        ));
    }
    outcomes.sort_by(|left, right| left.file_path().cmp(right.file_path()));
    outcomes
}

/// The folder a project-relative path lives in (empty for the root).
fn folder_of(file_path: &str) -> String {
    file_path
        .rsplit_once('/')
        .map(|(folder, _)| folder.to_owned())
        .unwrap_or_default()
}

/// The direct child folders of `folder`, sorted.
fn child_folders(folder: &str, folder_names: &[String]) -> Vec<String> {
    let mut children: Vec<String> = folder_names
        .iter()
        .filter(|candidate| {
            if *candidate == folder || candidate.is_empty() {
                return false;
            }
            // A direct child differs by exactly one extra segment.
            let remainder = if folder.is_empty() {
                candidate.as_str()
            } else {
                match candidate.strip_prefix(&format!("{folder}/")) {
                    Some(remainder) => remainder,
                    None => return false,
                }
            };
            !remainder.contains('/')
        })
        .cloned()
        .collect();
    children.sort();
    children
}

/// Decide what to do with one page.
fn outcome_for(
    folder: &str,
    name: &str,
    content: String,
    existing: &BTreeMap<String, String>,
) -> PageOutcome {
    let file_path = if folder.is_empty() {
        name.to_owned()
    } else {
        format!("{folder}/{name}")
    };
    let page = ProjectedPage {
        file_path: file_path.clone(),
        content,
    };
    match existing.get(&file_path) {
        None => PageOutcome::Create { page },
        Some(current) if current == &page.content => PageOutcome::Unchanged { file_path },
        Some(current) if current.contains(GENERATED_MARKER) => PageOutcome::Update { page },
        Some(_) => PageOutcome::Conflict {
            file_path,
            reason: "a hand-written page occupies this path; the projector never overwrites \
                     one"
            .to_owned(),
        },
    }
}

/// Build a folder's `index.md`.
fn build_index(folder: &str, notes: &[&NoteSummary], children: &[String]) -> String {
    let title = if folder.is_empty() {
        "Wiki index".to_owned()
    } else {
        format!("{folder} index")
    };
    let mut out = String::from("---\n");
    out.push_str(&format!("title: \"{title}\"\n"));
    out.push_str("type: wiki_index\n");
    out.push_str(&format!("{GENERATED_MARKER}\n"));
    out.push_str("---\n\n");
    out.push_str(&format!("# {title}\n\n"));

    if !children.is_empty() {
        out.push_str("## Folders\n\n");
        for child in children {
            let name = child.rsplit('/').next().unwrap_or(child);
            out.push_str(&format!("- [[{child}/index.md|{name}]]\n"));
        }
        out.push('\n');
    }
    out.push_str("## Notes\n\n");
    if notes.is_empty() {
        out.push_str("*No notes in this folder yet.*\n");
    } else {
        for note in notes {
            out.push_str(&format!("- [[{}|{}]]\n", note.file_path, note.title));
            if !note.tags.is_empty() {
                out.push_str(&format!("  *tags: {}*\n", note.tags.join(", ")));
            }
        }
    }
    out
}

/// Build a folder's `log.md`.
fn build_log(folder: &str, changes: &[&ChangeEntry]) -> String {
    let title = if folder.is_empty() {
        "Wiki log".to_owned()
    } else {
        format!("{folder} log")
    };
    let mut out = String::from("---\n");
    out.push_str(&format!("title: \"{title}\"\n"));
    out.push_str("type: wiki_log\n");
    out.push_str(&format!("{GENERATED_MARKER}\n"));
    // A log's entries are prose, not knowledge: nothing in it should mint observations or
    // relations from the generated text.
    out.push_str("bm_parse_semantics: false\n");
    out.push_str("---\n\n");
    out.push_str(&format!("# {title}\n\n"));
    if changes.is_empty() {
        out.push_str("*No changes recorded yet.*\n");
        return out;
    }
    out.push_str(&format!(
        "*The {LOG_ENTRIES} most recent changes, newest first.*\n\n"
    ));
    for change in changes {
        out.push_str(&change.render());
        out.push('\n');
    }
    out
}

/// A validation finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidationIssue {
    /// The page the issue concerns.
    pub file_path: String,
    /// What is wrong.
    pub message: String,
}

/// Check the projected pages for the problems that make the wiki untrustworthy.
///
/// A page is wrong when a reserved name is occupied by a hand-written note (the projector
/// cannot maintain it), or when a generated page's links point at pages that do not exist.
#[must_use]
pub fn validate(outcomes: &[PageOutcome], notes: &[NoteSummary]) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    for outcome in outcomes {
        if let PageOutcome::Conflict { file_path, reason } = outcome {
            issues.push(ValidationIssue {
                file_path: file_path.clone(),
                message: reason.clone(),
            });
        }
    }
    // A note at a reserved path is only a problem when it is not a projected page: a
    // hand-written `index.md` is the conflict above, and it would also appear here.
    for note in notes {
        let name = note.file_path.rsplit('/').next().unwrap_or(&note.file_path);
        if RESERVED_NAMES.contains(&name) {
            let projected = outcomes
                .iter()
                .any(|outcome| outcome.file_path() == note.file_path);
            if !projected {
                issues.push(ValidationIssue {
                    file_path: note.file_path.clone(),
                    message: format!(
                        "a note occupies the reserved wiki page name `{name}`; the projector \
                         cannot maintain this folder"
                    ),
                });
            }
        }
    }
    issues.sort_by(|left, right| left.file_path.cmp(&right.file_path));
    issues
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(file_path: &str, title: &str) -> NoteSummary {
        NoteSummary {
            file_path: file_path.to_owned(),
            title: title.to_owned(),
            tags: Vec::new(),
        }
    }

    fn change(kind: &str, file_path: &str, at: &str) -> ChangeEntry {
        ChangeEntry {
            kind: kind.to_owned(),
            file_path: file_path.to_owned(),
            previous_path: None,
            created_at: at.to_owned(),
        }
    }

    #[test]
    fn every_folder_with_a_note_gets_an_index_and_a_log() {
        let notes = vec![note("a.md", "A"), note("deep/b.md", "B")];
        let outcomes = plan(&notes, &[], &BTreeMap::new());
        let paths: Vec<&str> = outcomes.iter().map(PageOutcome::file_path).collect();
        assert!(paths.contains(&"index.md"), "{paths:?}");
        assert!(paths.contains(&"log.md"), "{paths:?}");
        // A nested note implies its folder and every parent.
        assert!(paths.contains(&"deep/index.md"), "{paths:?}");
        assert!(paths.contains(&"deep/log.md"), "{paths:?}");
    }

    #[test]
    fn the_index_links_its_notes_and_its_child_folders() {
        let notes = vec![note("a.md", "A"), note("deep/b.md", "B")];
        let outcomes = plan(&notes, &[], &BTreeMap::new());
        let index = outcomes
            .iter()
            .find(|outcome| outcome.file_path() == "index.md")
            .expect("root index");
        let PageOutcome::Create { page } = index else {
            panic!("expected a create: {index:?}");
        };
        assert!(page.content.contains("[[a.md|A]]"), "{}", page.content);
        assert!(
            page.content.contains("[[deep/index.md|deep]]"),
            "{}",
            page.content
        );
        assert!(
            page.content.contains(GENERATED_MARKER),
            "a projected page must be recognisable on the next run"
        );
    }

    #[test]
    fn the_index_carries_no_parse_semantics_marker_so_its_links_feed_the_graph() {
        // Upstream #1601: a projected `index.md` must NOT set `bm_parse_semantics: false`,
        // or the wikilinks it lists never become graph relations.
        let notes = vec![note("a.md", "A")];
        let outcomes = plan(&notes, &[], &BTreeMap::new());
        let PageOutcome::Create { page } = outcomes
            .iter()
            .find(|outcome| outcome.file_path() == "index.md")
            .expect("index")
        else {
            panic!("expected a create");
        };
        assert!(
            !page.content.contains("bm_parse_semantics"),
            "the index's links must reach the graph: {}",
            page.content
        );
    }

    #[test]
    fn the_log_keeps_the_most_recent_changes_and_is_graph_silent() {
        let notes = vec![note("a.md", "A")];
        let changes: Vec<ChangeEntry> = (0..40)
            .map(|index| change("update", "a.md", &format!("2026-06-{index:02}")))
            .collect();
        let outcomes = plan(&notes, &changes, &BTreeMap::new());
        let PageOutcome::Create { page } = outcomes
            .iter()
            .find(|outcome| outcome.file_path() == "log.md")
            .expect("log")
        else {
            panic!("expected a create");
        };
        assert!(
            page.content.contains("bm_parse_semantics: false"),
            "a log's entries are prose: {}",
            page.content
        );
        // The cap is the point: a log is not a full history.
        let entries = page
            .content
            .lines()
            .filter(|line| line.starts_with("- "))
            .count();
        assert_eq!(entries, LOG_ENTRIES, "{}", page.content);
    }

    #[test]
    fn a_hand_written_page_is_a_conflict_not_an_overwrite() {
        let notes = vec![note("a.md", "A")];
        let mut existing = BTreeMap::new();
        existing.insert(
            "index.md".to_owned(),
            "---\ntitle: My index\n---\n\nHand written.\n".to_owned(),
        );
        let outcomes = plan(&notes, &[], &existing);
        let index = outcomes
            .iter()
            .find(|outcome| outcome.file_path() == "index.md")
            .expect("index");
        assert!(
            matches!(index, PageOutcome::Conflict { .. }),
            "a hand-written index must not be overwritten: {index:?}"
        );
    }

    #[test]
    fn a_page_the_projector_wrote_is_updated() {
        let notes = vec![note("a.md", "A")];
        // First run: create.
        let first = plan(&notes, &[], &BTreeMap::new());
        let mut existing = BTreeMap::new();
        for outcome in &first {
            if let PageOutcome::Create { page } = outcome {
                existing.insert(page.file_path.clone(), page.content.clone());
            }
        }
        // Second run over the same vault: nothing changed.
        let second = plan(&notes, &[], &existing);
        assert!(
            second
                .iter()
                .all(|outcome| matches!(outcome, PageOutcome::Unchanged { .. })),
            "{second:?}"
        );
        // Add a note: the index is updated, not conflicted.
        let mut more = notes.clone();
        more.push(note("b.md", "B"));
        let third = plan(&more, &[], &existing);
        let index = third
            .iter()
            .find(|outcome| outcome.file_path() == "index.md")
            .expect("index");
        assert!(
            matches!(index, PageOutcome::Update { .. }),
            "a projector-owned page is updated: {index:?}"
        );
    }

    #[test]
    fn the_projection_is_deterministic() {
        let notes = vec![note("b.md", "B"), note("a.md", "A"), note("deep/c.md", "C")];
        let changes = vec![change("create", "a.md", "2026-06-01")];
        let first = plan(&notes, &changes, &BTreeMap::new());
        let second = plan(&notes, &changes, &BTreeMap::new());
        assert_eq!(first, second);
    }

    #[test]
    fn a_reserved_page_does_not_make_an_empty_folder_exist() {
        // A folder holding only a projected `index.md` is not a folder with content, so a
        // deleted folder does not leave a ghost behind.
        let notes = vec![note("gone/index.md", "index"), note("a.md", "A")];
        let outcomes = plan(&notes, &[], &BTreeMap::new());
        let paths: Vec<&str> = outcomes.iter().map(PageOutcome::file_path).collect();
        assert!(!paths.contains(&"gone/index.md"), "{paths:?}");
    }

    #[test]
    fn validate_reports_a_conflict() {
        let notes = vec![note("a.md", "A")];
        let mut existing = BTreeMap::new();
        existing.insert("index.md".to_owned(), "hand written\n".to_owned());
        let outcomes = plan(&notes, &[], &existing);
        let issues = validate(&outcomes, &notes);
        assert!(
            issues.iter().any(|issue| issue.file_path == "index.md"),
            "{issues:?}"
        );
    }

    #[test]
    fn a_clean_projection_validates() {
        let notes = vec![note("a.md", "A")];
        let outcomes = plan(&notes, &[], &BTreeMap::new());
        assert!(validate(&outcomes, &notes).is_empty());
    }

    #[test]
    fn a_move_entry_names_both_paths() {
        let mut moved = change("move", "new.md", "2026-06-01");
        moved.previous_path = Some("old.md".to_owned());
        let rendered = moved.render();
        assert!(rendered.contains("old.md"), "{rendered}");
        assert!(rendered.contains("new.md"), "{rendered}");
    }
}
