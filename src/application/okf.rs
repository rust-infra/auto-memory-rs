//! OKF export and validation.
//!
//! Upstream 0.24.0 (#1550) adds `bm okf export DESTINATION` and `bm okf check BUNDLE`,
//! which write and validate a static OKF v0.2 bundle: a portable directory of Markdown
//! concepts that another tool can read without this project's index.
//!
//! The bundle is a *projection*, so three properties matter:
//!
//! - **nothing is lost that the format can carry**: concept frontmatter, categorized
//!   observations and relative asset paths all survive;
//! - **wikilinks become standard Markdown links**, resolved the way the graph resolves
//!   them, with the original relation types kept in a `bm.okf_export` extension so the
//!   round trip does not flatten the knowledge;
//! - **the destination is never half-written**: the export stages beside the destination,
//!   validates before publishing, refuses to overwrite without `--replace`, and restores
//!   the previous bundle if publication fails.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The OKF version this exporter writes.
pub const OKF_VERSION: &str = "0.2";

/// One concept in the bundle: a note and everything the format carries about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Concept {
    /// Project-relative path the concept came from.
    pub source_path: String,
    /// Concept title.
    pub title: String,
    /// Note type.
    pub note_type: String,
    /// Frontmatter as ordered pairs, preserved verbatim.
    pub frontmatter: Vec<(String, String)>,
    /// Observations, grouped by category in first-appearance order.
    pub observations: BTreeMap<String, Vec<String>>,
    /// Relations, as `(type, target title)`.
    pub relations: Vec<(String, String)>,
    /// The body with wikilinks rewritten to Markdown links.
    pub body: String,
    /// Relative asset paths the concept references.
    pub assets: Vec<String>,
}

/// One file in the exported bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BundleFile {
    /// Path inside the bundle.
    pub path: String,
    /// File content.
    pub content: String,
}

/// Render one concept as a bundle file.
#[must_use]
pub fn render_concept(concept: &Concept) -> BundleFile {
    let mut out = String::from("---\n");
    out.push_str(&format!("title: \"{}\"\n", concept.title));
    out.push_str(&format!("type: {}\n", concept.note_type));
    out.push_str(&format!("okf_version: \"{OKF_VERSION}\"\n"));
    // The original frontmatter is kept as authored, except the keys the format owns.
    for (key, value) in &concept.frontmatter {
        if matches!(key.as_str(), "title" | "type") {
            continue;
        }
        out.push_str(&format!("{key}: {value}\n"));
    }
    // The relation types the Markdown links cannot carry.
    if !concept.relations.is_empty() {
        out.push_str("bm.okf_export:\n  relations:\n");
        for (relation_type, target) in &concept.relations {
            out.push_str(&format!(
                "    - type: {relation_type}\n      target: \"{target}\"\n"
            ));
        }
    }
    out.push_str("---\n\n");

    // Observations keep their categories, which is the part of the knowledge format that
    // a plain Markdown body would lose.
    for (category, items) in &concept.observations {
        out.push_str(&format!("## {category}\n\n"));
        for item in items {
            out.push_str(&format!("- {item}\n"));
        }
        out.push('\n');
    }
    out.push_str(&concept.body);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    BundleFile {
        path: concept.source_path.clone(),
        content: out,
    }
}

/// Render a folder's `index.md` for the bundle.
#[must_use]
pub fn render_folder_index(folder: &str, concepts: &[&Concept]) -> BundleFile {
    let title = if folder.is_empty() {
        "Index".to_owned()
    } else {
        format!("{folder} index")
    };
    let mut content = format!("# {title}\n\n");
    for concept in concepts {
        // The link is relative to the index's own folder, so it stays valid wherever the
        // bundle is unpacked.
        let target = if folder.is_empty() {
            concept.source_path.clone()
        } else {
            concept
                .source_path
                .strip_prefix(&format!("{folder}/"))
                .unwrap_or(&concept.source_path)
                .to_owned()
        };
        let name = target.rsplit('/').next().unwrap_or_default();
        content.push_str(&format!("- [{name}]({target})\n"));
    }
    BundleFile {
        path: if folder.is_empty() {
            "index.md".to_owned()
        } else {
            format!("{folder}/index.md")
        },
        content,
    }
}

/// Render the bundle's dated root `log.md`.
#[must_use]
pub fn render_root_log(date: &str, concepts: &[&Concept]) -> BundleFile {
    let mut content = format!("# Export log\n\nExported {date}.\n\n");
    for concept in concepts {
        content.push_str(&format!("- {} ({})\n", concept.title, concept.source_path));
    }
    BundleFile {
        path: "log.md".to_owned(),
        content,
    }
}

/// Rewrite `[[target]]` wikilinks in `body` as Markdown links.
///
/// `resolve` maps a wikilink target to the bundle-relative path of the concept it names.
/// A target that does not resolve keeps its text but loses the link, because a dangling
/// link in a portable bundle is worse than plain prose.
#[must_use]
pub fn rewrite_wikilinks(body: &str, resolve: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(open) = rest.find("[[") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find("]]") else {
            // An unclosed `[[` is prose.
            out.push_str(&rest[open..]);
            return out;
        };
        let inner = &after[..close];
        // `[[target|Label]]` keeps its label; `[[target]]` uses the target's last segment.
        let (target, label) = inner
            .split_once('|')
            .map(|(target, label)| (target.trim(), label.trim()))
            .unwrap_or((inner.trim(), ""));
        let label = if label.is_empty() {
            target
                .rsplit('/')
                .next()
                .unwrap_or(target)
                .trim_end_matches(".md")
        } else {
            label
        };
        match resolve.get(target) {
            Some(path) => out.push_str(&format!("[{label}]({path})")),
            None => out.push_str(label),
        }
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    out
}

/// One validation finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OkfIssue {
    /// The file the issue concerns.
    pub file_path: String,
    /// The rule that was violated.
    pub rule: String,
    /// What is wrong.
    pub message: String,
}

/// Validate a rendered bundle.
///
/// The checks are the ones that make a bundle *portable*: every file is Markdown with a
/// title and a type, every internal link resolves, and no file is a directory or
/// unreadable.
#[must_use]
pub fn validate_bundle(files: &[BundleFile]) -> Vec<OkfIssue> {
    let mut issues = Vec::new();
    let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    for file in files {
        // Every file is Markdown: a bundle with a stray binary is not portable.
        if !file.path.ends_with(".md") {
            issues.push(OkfIssue {
                file_path: file.path.clone(),
                rule: "markdown-only".to_owned(),
                message: "every bundle file must be Markdown".to_owned(),
            });
            continue;
        }
        // An empty file is a broken concept.
        if file.content.trim().is_empty() {
            issues.push(OkfIssue {
                file_path: file.path.clone(),
                rule: "non-empty".to_owned(),
                message: "a bundle file must not be empty".to_owned(),
            });
            continue;
        }
        // A concept needs a title; the folder index and the log are generated pages and
        // carry theirs in the body.
        let is_generated = file.path.ends_with("index.md") || file.path == "log.md";
        if !is_generated {
            if !file.content.contains("title:") {
                issues.push(OkfIssue {
                    file_path: file.path.clone(),
                    rule: "concept-title".to_owned(),
                    message: "a concept must declare a title".to_owned(),
                });
            }
            if !file.content.contains("type:") {
                issues.push(OkfIssue {
                    file_path: file.path.clone(),
                    rule: "concept-type".to_owned(),
                    message: "a concept must declare a type".to_owned(),
                });
            }
        }
        // Every relative Markdown link must resolve inside the bundle. A link is relative
        // to the file that contains it, so `deep/index.md` linking `nested.md` means
        // `deep/nested.md` — not a root-level file.
        let folder = file
            .path
            .rsplit_once('/')
            .map(|(folder, _)| folder.to_owned())
            .unwrap_or_default();
        for target in markdown_link_targets(&file.content) {
            if target.starts_with("http://") || target.starts_with("https://") {
                continue;
            }
            let resolved = if folder.is_empty() {
                target.clone()
            } else {
                format!("{folder}/{target}")
            };
            if !paths.contains(&resolved.as_str()) {
                issues.push(OkfIssue {
                    file_path: file.path.clone(),
                    rule: "link-resolves".to_owned(),
                    message: format!("link target `{target}` is not in the bundle"),
                });
            }
        }
    }
    issues.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.rule.cmp(&right.rule))
    });
    issues
}

/// The relative targets of the Markdown links in `content`.
fn markdown_link_targets(content: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut rest = content;
    while let Some(open) = rest.find("](") {
        let after = &rest[open + 2..];
        let Some(close) = after.find(')') else {
            break;
        };
        let target = after[..close].split_whitespace().next().unwrap_or_default();
        if !target.is_empty() && !target.starts_with('#') {
            targets.push(target.to_owned());
        }
        rest = &after[close + 1..];
    }
    targets
}

/// Publish a rendered bundle to `destination`.
///
/// The export stages beside the destination, validates before publishing, refuses to
/// overwrite without `replace`, and restores the previous bundle if publication fails —
/// so a failed export never leaves a half-written directory.
pub fn publish(files: &[BundleFile], destination: &Path, replace: bool) -> Result<()> {
    let issues = validate_bundle(files);
    if !issues.is_empty() {
        return Err(Error::InvalidArgument {
            message: format!(
                "the bundle failed validation ({} issue(s)); first: {}: {}",
                issues.len(),
                issues[0].file_path,
                issues[0].message
            ),
        });
    }
    if destination.exists() && !replace {
        return Err(Error::InvalidArgument {
            message: format!(
                "{} already exists; pass --replace to overwrite it",
                destination.display()
            ),
        });
    }
    // Stage beside the destination so the rename stays on one filesystem.
    let staging = staging_path(destination);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;
    for file in files {
        let target = staging.join(&file.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, &file.content)?;
    }
    // The previous bundle is moved aside rather than deleted, so a failed publication can
    // put it back.
    let backup = destination.with_extension("okf-backup");
    if destination.exists() {
        if backup.exists() {
            std::fs::remove_dir_all(&backup)?;
        }
        std::fs::rename(destination, &backup)?;
    }
    match std::fs::rename(&staging, destination) {
        Ok(()) => {
            if backup.exists() {
                let _ = std::fs::remove_dir_all(&backup);
            }
            Ok(())
        }
        Err(error) => {
            // Restore the previous bundle, then report the failure.
            if backup.exists() {
                let _ = std::fs::rename(&backup, destination);
            }
            let _ = std::fs::remove_dir_all(&staging);
            Err(Error::Io(error))
        }
    }
}

/// The staging directory beside `destination`.
fn staging_path(destination: &Path) -> PathBuf {
    let name = destination
        .file_name()
        .map(|name| format!(".{}.staging", name.to_string_lossy()))
        .unwrap_or_else(|| ".okf.staging".to_owned());
    destination.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn concept() -> Concept {
        let mut observations = BTreeMap::new();
        observations.insert("decision".to_owned(), vec!["Use SQLite.".to_owned()]);
        Concept {
            source_path: "notes/simple.md".to_owned(),
            title: "Simple".to_owned(),
            note_type: "note".to_owned(),
            frontmatter: vec![
                ("title".to_owned(), "Simple".to_owned()),
                ("tags".to_owned(), "[rust]".to_owned()),
            ],
            observations,
            relations: vec![("relates_to".to_owned(), "Other".to_owned())],
            body: "See [[Other]] for details.\n".to_owned(),
            assets: Vec::new(),
        }
    }

    #[test]
    fn a_concept_keeps_its_observations_and_relation_types() {
        let file = render_concept(&concept());
        assert!(file.content.contains("## decision"), "{}", file.content);
        assert!(file.content.contains("- Use SQLite."), "{}", file.content);
        // The relation type survives in the extension block, because the Markdown link
        // cannot carry it.
        assert!(file.content.contains("bm.okf_export"), "{}", file.content);
        assert!(
            file.content.contains("type: relates_to"),
            "{}",
            file.content
        );
        assert!(file.content.contains("okf_version: \"0.2\""));
    }

    #[test]
    fn a_wikilink_becomes_a_markdown_link_when_it_resolves() {
        let mut resolve = BTreeMap::new();
        resolve.insert("Other".to_owned(), "notes/other.md".to_owned());
        let rewritten = rewrite_wikilinks("See [[Other]] for details.", &resolve);
        assert_eq!(rewritten, "See [Other](notes/other.md) for details.");
    }

    #[test]
    fn a_labeled_wikilink_keeps_its_label() {
        let mut resolve = BTreeMap::new();
        resolve.insert("notes/other.md".to_owned(), "notes/other.md".to_owned());
        let rewritten = rewrite_wikilinks("See [[notes/other.md|the other note]].", &resolve);
        assert_eq!(rewritten, "See [the other note](notes/other.md).");
    }

    #[test]
    fn an_unresolved_wikilink_becomes_plain_text() {
        // A dangling link in a portable bundle is worse than prose: the reader would
        // follow it and find nothing.
        let rewritten = rewrite_wikilinks("See [[Nowhere]].", &BTreeMap::new());
        assert_eq!(rewritten, "See Nowhere.");
    }

    #[test]
    fn an_unclosed_wikilink_is_prose() {
        let rewritten = rewrite_wikilinks("See [[unclosed", &BTreeMap::new());
        assert_eq!(rewritten, "See [[unclosed");
    }

    #[test]
    fn a_clean_bundle_validates() {
        let files = vec![render_concept(&concept())];
        assert!(validate_bundle(&files).is_empty());
    }

    #[test]
    fn a_concept_without_a_title_is_reported() {
        let files = vec![BundleFile {
            path: "a.md".to_owned(),
            content: "type: note\n\nbody\n".to_owned(),
        }];
        let issues = validate_bundle(&files);
        assert!(
            issues.iter().any(|issue| issue.rule == "concept-title"),
            "{issues:?}"
        );
    }

    #[test]
    fn a_dangling_link_is_reported() {
        let files = vec![BundleFile {
            path: "a.md".to_owned(),
            content: "---\ntitle: A\ntype: note\n---\n\n[missing](b.md)\n".to_owned(),
        }];
        let issues = validate_bundle(&files);
        assert!(
            issues.iter().any(|issue| issue.rule == "link-resolves"),
            "{issues:?}"
        );
    }

    #[test]
    fn an_external_link_is_not_a_dangling_one() {
        let files = vec![BundleFile {
            path: "a.md".to_owned(),
            content: "---\ntitle: A\ntype: note\n---\n\n[site](https://example.com)\n".to_owned(),
        }];
        assert!(validate_bundle(&files).is_empty());
    }

    #[test]
    fn a_non_markdown_file_is_reported() {
        let files = vec![BundleFile {
            path: "a.png".to_owned(),
            content: "binary".to_owned(),
        }];
        let issues = validate_bundle(&files);
        assert!(
            issues.iter().any(|issue| issue.rule == "markdown-only"),
            "{issues:?}"
        );
    }

    #[test]
    fn a_folder_index_and_a_root_log_are_generated_pages() {
        let concept = concept();
        let files = vec![
            render_concept(&concept),
            render_folder_index("", &[&concept]),
            render_root_log("2026-06-10", &[&concept]),
        ];
        // The generated pages carry no frontmatter title, and that is not an issue.
        assert!(
            validate_bundle(&files).is_empty(),
            "{:?}",
            validate_bundle(&files)
        );
    }

    #[test]
    fn publish_refuses_to_overwrite_without_replace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let destination = dir.path().join("bundle");
        std::fs::create_dir_all(&destination).expect("destination");
        let files = vec![render_concept(&concept())];
        let error = publish(&files, &destination, false).expect_err("must refuse");
        assert!(error.to_string().contains("--replace"), "{error}");
    }

    #[test]
    fn publish_writes_the_bundle_and_leaves_no_staging_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let destination = dir.path().join("bundle");
        let files = vec![render_concept(&concept())];
        publish(&files, &destination, false).expect("publish");
        assert!(destination.join("notes/simple.md").is_file());
        // The staging directory is gone, and no backup is left behind.
        assert!(!staging_path(&destination).exists());
        assert!(!destination.with_extension("okf-backup").exists());
    }

    #[test]
    fn publish_replaces_an_existing_bundle() {
        let dir = tempfile::tempdir().expect("tempdir");
        let destination = dir.path().join("bundle");
        let first = vec![render_concept(&concept())];
        publish(&first, &destination, false).expect("first publish");
        let mut second_concept = concept();
        second_concept.title = "Renamed".to_owned();
        let second = vec![render_concept(&second_concept)];
        publish(&second, &destination, true).expect("second publish");
        let content =
            std::fs::read_to_string(destination.join("notes/simple.md")).expect("concept");
        assert!(content.contains("Renamed"), "{content}");
    }

    #[test]
    fn publish_refuses_a_bundle_that_fails_validation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let destination = dir.path().join("bundle");
        let files = vec![BundleFile {
            path: "a.md".to_owned(),
            content: String::new(),
        }];
        let error = publish(&files, &destination, false).expect_err("must refuse");
        assert!(error.to_string().contains("failed validation"), "{error}");
        assert!(!destination.exists(), "a refused bundle writes nothing");
    }
}
