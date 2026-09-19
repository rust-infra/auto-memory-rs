//! Directory listing (`list_directory`).
//!
//! Ports the reference `DirectoryService.list_directory` plus the MCP tool's text
//! renderer. The reference builds a directory tree from the entities under a path
//! prefix, flattens it to a requested depth, sorts it, and returns one bounded page.
//! Two details are load-bearing and easy to lose:
//!
//! * the glob gates *inclusion in the result* but never recursion, so `*.md` still
//!   descends into directories whose names cannot match;
//! * paged nodes are always emitted with `children: []`, so the response can never
//!   smuggle an unbounded subtree past the page bound.

use std::collections::HashMap;

use serde::Serialize;
use strum::{EnumString, IntoStaticStr};

use crate::error::{Error, Result};
use crate::storage::{DirectoryEntityRow, Store};

/// Default page size (`DEFAULT_DIRECTORY_PAGE_SIZE`).
pub const DEFAULT_DIRECTORY_PAGE_SIZE: u32 = 10;
/// Largest accepted page size (`MAX_DIRECTORY_PAGE_SIZE`).
pub const MAX_DIRECTORY_PAGE_SIZE: u32 = 200;

/// Explicit ordering requested by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum DirectorySortOrder {
    /// Title, ascending.
    TitleAsc,
    /// Title, descending.
    TitleDesc,
    /// Last-modified time, ascending.
    UpdatedAsc,
    /// Last-modified time, descending.
    UpdatedDesc,
}

impl DirectorySortOrder {
    /// Parse the reference's `Literal["title_asc", "title_desc", "updated_asc",
    /// "updated_desc"]`; `strum` maps the names, this keeps the reference's error
    /// text for an unknown one.
    pub fn parse(value: &str) -> Result<Self> {
        value.parse().map_err(|_| Error::InvalidArgument {
            message: format!("invalid sort: {value}"),
        })
    }
}

/// One node in a directory listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirectoryNode {
    /// File or directory name.
    pub name: String,
    /// Original file path without a leading slash (files only).
    pub file_path: Option<String>,
    /// Path with a leading slash, used for navigation.
    pub directory_path: String,
    /// `directory` or `file`.
    #[serde(rename = "type")]
    pub node_type: String,
    /// Always empty in a paged response.
    pub children: Vec<DirectoryNode>,
    /// Note title (files only).
    pub title: Option<String>,
    /// Note permalink (files only).
    pub permalink: Option<String>,
    /// Stable external identifier.
    pub external_id: Option<String>,
    /// Internal numeric id.
    pub entity_id: Option<i64>,
    /// Note type.
    pub note_type: Option<String>,
    /// Stored content type.
    pub content_type: Option<String>,
    /// Last-modified timestamp.
    pub updated_at: Option<String>,
}

impl DirectoryNode {
    fn directory(name: &str, directory_path: &str) -> Self {
        Self {
            name: name.to_owned(),
            file_path: None,
            directory_path: directory_path.to_owned(),
            node_type: "directory".to_owned(),
            children: Vec::new(),
            title: None,
            permalink: None,
            external_id: None,
            entity_id: None,
            note_type: None,
            content_type: None,
            updated_at: None,
        }
    }

    fn is_directory(&self) -> bool {
        self.node_type == "directory"
    }
}

/// One bounded page of directory-listing results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirectoryListResponse {
    /// Nodes on this page.
    pub nodes: Vec<DirectoryNode>,
    /// One-indexed page number.
    pub page: u32,
    /// Requested page size.
    pub page_size: u32,
    /// Total matches before pagination.
    pub total: usize,
    /// Whether more pages follow.
    pub has_more: bool,
}

/// Arguments for [`list_directory`].
#[derive(Debug, Clone)]
pub struct DirectoryOptions {
    /// Directory path, default `/`.
    pub dir_name: String,
    /// Recursion depth (1 = immediate children).
    pub depth: u32,
    /// Optional glob matched against file names.
    pub file_name_glob: Option<String>,
    /// Optional explicit ordering.
    pub sort: Option<DirectorySortOrder>,
    /// One-indexed page.
    pub page: u32,
    /// Page size.
    pub page_size: u32,
}

impl Default for DirectoryOptions {
    fn default() -> Self {
        Self {
            dir_name: "/".to_owned(),
            depth: 1,
            file_name_glob: None,
            sort: None,
            page: 1,
            page_size: DEFAULT_DIRECTORY_PAGE_SIZE,
        }
    }
}

/// List one page of a vault directory.
pub fn list_directory(
    store: &Store,
    project_id: i64,
    options: &DirectoryOptions,
) -> Result<DirectoryListResponse> {
    if options.page < 1 {
        return Err(Error::InvalidArgument {
            message: format!("page must be >= 1, got {}", options.page),
        });
    }
    if options.page_size < 1 {
        return Err(Error::InvalidArgument {
            message: format!("page_size must be >= 1, got {}", options.page_size),
        });
    }
    if options.page_size > MAX_DIRECTORY_PAGE_SIZE {
        return Err(Error::InvalidArgument {
            message: format!(
                "page_size must be <= {MAX_DIRECTORY_PAGE_SIZE}, got {}",
                options.page_size
            ),
        });
    }

    let dir_name = normalize_dir_name(&options.dir_name);
    let rows = store.directory_rows(project_id, dir_name.trim_matches('/'))?;
    let arena = Arena::build(&rows, &dir_name);
    let mut result = Vec::new();
    arena.collect(
        0,
        &mut result,
        options.depth,
        options.file_name_glob.as_deref(),
        0,
    );

    let mut ordered: Vec<DirectoryNode> = result
        .iter()
        .map(|index| arena.nodes[*index].node.clone())
        .collect();
    sort_nodes(&mut ordered, options.sort)?;

    let total = ordered.len();
    let start = ((options.page - 1) as usize).saturating_mul(options.page_size as usize);
    let end = start.saturating_add(options.page_size as usize);
    let nodes = ordered
        .into_iter()
        .skip(start)
        .take(options.page_size as usize)
        .map(|mut node| {
            // Paged nodes never carry their subtree: the reference clears `children`
            // so the page bound cannot be bypassed during serialization.
            node.children = Vec::new();
            node
        })
        .collect();

    Ok(DirectoryListResponse {
        nodes,
        page: options.page,
        page_size: options.page_size,
        total,
        has_more: end < total,
    })
}

/// Render the reference's human-readable listing.
pub fn render_directory_text(
    listing: &DirectoryListResponse,
    options: &DirectoryOptions,
    dir_name: &str,
) -> String {
    let dir_name = normalize_dir_name(dir_name);
    let glob = options.file_name_glob.as_deref();
    if listing.total == 0 {
        let filter_desc = glob.map_or_else(String::new, |glob| format!(" matching '{glob}'"));
        return format!("No files found in directory '{dir_name}'{filter_desc}");
    }

    let mut lines = Vec::new();
    match glob {
        Some(glob) => lines.push(format!(
            "Files in '{dir_name}' matching '{glob}' (depth {}):",
            options.depth
        )),
        None => lines.push(format!(
            "Contents of '{dir_name}' (depth {}):",
            options.depth
        )),
    }
    lines.push(format!(
        "Page {} (page size {}, {} total items)",
        listing.page, listing.page_size, listing.total
    ));
    lines.push(String::new());

    let directories: Vec<&DirectoryNode> = listing
        .nodes
        .iter()
        .filter(|node| node.is_directory())
        .collect();
    let files: Vec<&DirectoryNode> = listing
        .nodes
        .iter()
        .filter(|node| !node.is_directory())
        .collect();

    if listing.nodes.is_empty() {
        lines.push("No items on this page.".to_owned());
    }
    for node in &directories {
        lines.push(format!(
            "\u{1f4c1} {:<30} {}",
            node.name, node.directory_path
        ));
    }
    if !directories.is_empty() && !files.is_empty() {
        lines.push(String::new());
    }
    for node in &files {
        let mut path_display = node.directory_path.clone();
        if let Some(stripped) = path_display.strip_prefix('/') {
            path_display = stripped.to_owned();
        }
        let title = node.title.clone().unwrap_or_default();
        let updated = node.updated_at.clone().unwrap_or_default();

        let mut line = format!("\u{1f4c4} {:<30} {path_display}", node.name);
        if !title.is_empty() && title != node.name {
            line.push_str(&format!(" | {title}"));
        }
        if let Some(date) = display_date(&updated) {
            line.push_str(&format!(" | {date}"));
        }
        if let Some(external_id) = node.external_id.as_deref() {
            line.push_str(&format!(" | id: {external_id}"));
        }
        lines.push(line);
    }

    lines.push(String::new());
    let mut summary_parts = Vec::new();
    if !directories.is_empty() {
        let noun = if directories.len() == 1 {
            "directory"
        } else {
            "directories"
        };
        summary_parts.push(format!("{} {noun}", directories.len()));
    }
    if !files.is_empty() {
        let noun = if files.len() == 1 { "file" } else { "files" };
        summary_parts.push(format!("{} {noun}", files.len()));
    }
    let total_count = directories.len() + files.len();
    if summary_parts.is_empty() {
        lines.push("Total: 0 items".to_owned());
    } else {
        lines.push(format!(
            "Total: {total_count} items ({})",
            summary_parts.join(", ")
        ));
    }

    if listing.nodes.is_empty() {
        let last_page = listing.total.div_ceil(listing.page_size as usize);
        lines.push(format!(
            "Requested page {} is beyond the available results; the last available page is {last_page}.",
            listing.page
        ));
    }

    if listing.has_more {
        lines.push(String::new());
        let mut continuation = vec![
            format!("dir_name={}", crate::pycompat::python_repr(&dir_name)),
            format!("depth={}", options.depth),
            format!("page={}", listing.page + 1),
            format!("page_size={}", listing.page_size),
        ];
        if let Some(glob) = glob {
            continuation.push(format!(
                "file_name_glob={}",
                crate::pycompat::python_repr(glob)
            ));
        }
        if let Some(sort) = options.sort {
            continuation.push(format!(
                "sort={}",
                crate::pycompat::python_repr(<&str>::from(sort))
            ));
        }
        lines.push(format!(
            "More results available. Call list_directory({}) to continue.",
            continuation.join(", ")
        ));
    }

    lines.join("\n")
}

/// Normalize a caller path the way `DirectoryService.list_directory` does.
fn normalize_dir_name(dir_name: &str) -> String {
    let mut normalized = dir_name.strip_prefix("./").unwrap_or(dir_name).to_owned();
    if !normalized.starts_with('/') {
        normalized = format!("/{normalized}");
    }
    if normalized != "/" {
        while normalized.ends_with('/') {
            normalized.pop();
        }
    }
    normalized
}

/// One arena slot: the emitted node plus its children's slots.
///
/// The reference keeps real child lists; we keep indices so the traversal order is
/// unambiguous and cloning for the page happens only at the end.
struct TreeEntry {
    node: DirectoryNode,
    children: Vec<usize>,
}

/// Arena used to reproduce the reference's insertion order exactly.
struct Arena {
    nodes: Vec<TreeEntry>,
    index_by_path: HashMap<String, usize>,
}

impl Arena {
    /// Rebuild `_build_directory_tree_from_entities` in the same two passes.
    fn build(rows: &[DirectoryEntityRow], root_path: &str) -> Self {
        let mut arena = Self {
            nodes: vec![TreeEntry {
                node: DirectoryNode::directory("Root", root_path),
                children: Vec::new(),
            }],
            index_by_path: HashMap::from([(root_path.to_owned(), 0)]),
        };
        for row in rows {
            let parts = row
                .file_path
                .split('/')
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>();
            let mut current_path = "/".to_owned();
            for part in parts.iter().take(parts.len().saturating_sub(1)) {
                let parent_path = current_path.clone();
                current_path = if current_path == "/" {
                    format!("{current_path}{part}")
                } else {
                    format!("{current_path}/{part}")
                };
                if arena.index_by_path.contains_key(&current_path) {
                    continue;
                }
                let index = arena.nodes.len();
                arena.nodes.push(TreeEntry {
                    node: DirectoryNode::directory(part, &current_path),
                    children: Vec::new(),
                });
                arena.index_by_path.insert(current_path.clone(), index);
                if let Some(parent) = arena.index_by_path.get(&parent_path).copied() {
                    arena.nodes[parent].children.push(index);
                }
            }
        }
        for row in rows {
            let file_name = row
                .file_path
                .rsplit('/')
                .next()
                .unwrap_or(&row.file_path)
                .to_owned();
            let parent_dir = row.file_path.rsplit_once('/').map_or("", |(dir, _)| dir);
            let directory_path = if parent_dir.is_empty() {
                "/".to_owned()
            } else {
                format!("/{parent_dir}")
            };
            let index = arena.nodes.len();
            arena.nodes.push(TreeEntry {
                node: DirectoryNode {
                    name: file_name,
                    file_path: Some(row.file_path.clone()),
                    directory_path: format!("/{}", row.file_path),
                    node_type: "file".to_owned(),
                    children: Vec::new(),
                    title: Some(row.title.clone()),
                    permalink: row.permalink.clone(),
                    external_id: Some(row.external_id.clone()),
                    entity_id: Some(row.id),
                    note_type: Some(row.note_type.clone()),
                    content_type: Some(row.content_type.clone()),
                    updated_at: Some(row.updated_at.clone()),
                },
                children: Vec::new(),
            });
            if let Some(parent) = arena.index_by_path.get(&directory_path).copied() {
                arena.nodes[parent].children.push(index);
            } else if let Some(root) = arena.index_by_path.get(root_path).copied() {
                arena.nodes[root].children.push(index);
            }
        }
        arena
    }

    fn collect(
        &self,
        node: usize,
        result: &mut Vec<usize>,
        max_depth: u32,
        glob: Option<&str>,
        current_depth: u32,
    ) {
        if current_depth >= max_depth {
            return;
        }
        for child in self.nodes[node].children.clone() {
            let entry = &self.nodes[child].node;
            if glob.is_none_or(|glob| glob_match(glob, &entry.name)) {
                result.push(child);
            }
            if entry.is_directory() && current_depth < max_depth {
                self.collect(child, result, max_depth, glob, current_depth + 1);
            }
        }
    }
}

/// Reproduce the reference's sort branches, including its stable tie-breaking.
fn sort_nodes(nodes: &mut [DirectoryNode], sort: Option<DirectorySortOrder>) -> Result<()> {
    match sort {
        None => nodes.sort_by(|left, right| {
            directory_rank(left)
                .cmp(&directory_rank(right))
                .then_with(|| fold(&left.name).cmp(&fold(&right.name)))
                .then_with(|| fold(&left.directory_path).cmp(&fold(&right.directory_path)))
                .then_with(|| left.directory_path.cmp(&right.directory_path))
        }),
        Some(order) => {
            let descending = matches!(
                order,
                DirectorySortOrder::TitleDesc | DirectorySortOrder::UpdatedDesc
            );
            let mut directories: Vec<DirectoryNode> = nodes
                .iter()
                .filter(|node| node.is_directory())
                .cloned()
                .collect();
            let mut files: Vec<DirectoryNode> = nodes
                .iter()
                .filter(|node| !node.is_directory())
                .cloned()
                .collect();
            // Python's `sort(reverse=True)` is stable: equal keys keep their previous
            // order. Reversing the finished list instead would flip ties as well, so
            // every descending branch below inverts the comparator and nothing else.
            let directory_key = |left: &DirectoryNode, right: &DirectoryNode| {
                fold(&left.name)
                    .cmp(&fold(&right.name))
                    .then_with(|| fold(&left.directory_path).cmp(&fold(&right.directory_path)))
                    .then_with(|| left.directory_path.cmp(&right.directory_path))
            };
            directories.sort_by(|left, right| {
                if order == DirectorySortOrder::TitleDesc {
                    directory_key(right, left)
                } else {
                    directory_key(left, right)
                }
            });
            match order {
                DirectorySortOrder::TitleAsc | DirectorySortOrder::TitleDesc => {
                    files.sort_by(|left, right| {
                        if descending {
                            file_identity_key(right, left)
                        } else {
                            file_identity_key(left, right)
                        }
                    });
                }
                DirectorySortOrder::UpdatedAsc | DirectorySortOrder::UpdatedDesc => {
                    // Order by identity first so equal timestamps stay put across
                    // pages; the timestamp sort below is stable and preserves it.
                    files.sort_by(file_identity_key);
                    for node in &files {
                        if node.updated_at.is_none() {
                            return Err(Error::InvalidArgument {
                                message: format!(
                                    "File directory node '{}' is missing updated_at",
                                    node.directory_path
                                ),
                            });
                        }
                    }
                    let updated_key =
                        |node: &DirectoryNode| node.updated_at.clone().unwrap_or_default();
                    files.sort_by(|left, right| {
                        if descending {
                            updated_key(right).cmp(&updated_key(left))
                        } else {
                            updated_key(left).cmp(&updated_key(right))
                        }
                    });
                }
            }
            directories.extend(files);
            for (slot, node) in nodes.iter_mut().zip(directories) {
                *slot = node;
            }
        }
    }
    Ok(())
}

fn directory_rank(node: &DirectoryNode) -> u8 {
    u8::from(!node.is_directory())
}

fn file_identity_key(left: &DirectoryNode, right: &DirectoryNode) -> std::cmp::Ordering {
    let left_title = fold(left.title.as_deref().unwrap_or(&left.name));
    let right_title = fold(right.title.as_deref().unwrap_or(&right.name));
    left_title
        .cmp(&right_title)
        .then_with(|| fold(&left.directory_path).cmp(&fold(&right.directory_path)))
        .then_with(|| left.external_id.cmp(&right.external_id))
}

/// Python `str.casefold()` is full Unicode case folding; `to_lowercase` is the
/// closest stable approximation available without a Unicode folding table.
fn fold(value: &str) -> String {
    value.to_lowercase()
}

/// Format an indexed timestamp as `YYYY-MM-DD`, like the reference `strftime`.
fn display_date(updated: &str) -> Option<String> {
    if updated.is_empty() {
        return None;
    }
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(updated) {
        return Some(parsed.format("%Y-%m-%d").to_string());
    }
    if updated.len() >= 10 {
        return Some(updated[..10].to_owned());
    }
    None
}

/// Port of `fnmatch.fnmatch` for the subset the reference tool documents.
///
/// POSIX `fnmatch` is case-sensitive, `*` and `?` are wildcards, `[seq]` is a
/// character class, and `[!seq]` negates it. A `[` without a closing bracket is a
/// literal, matching Python's translation.
fn glob_match(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    match_from(&pattern, &name)
}

fn match_from(pattern: &[char], name: &[char]) -> bool {
    let mut pattern_index = 0;
    let mut name_index = 0;
    while pattern_index < pattern.len() {
        match pattern[pattern_index] {
            '*' => {
                // Collapse consecutive stars, then try every split point.
                while pattern_index + 1 < pattern.len() && pattern[pattern_index + 1] == '*' {
                    pattern_index += 1;
                }
                if pattern_index + 1 == pattern.len() {
                    return true;
                }
                for skip in name_index..=name.len() {
                    if match_from(&pattern[pattern_index + 1..], &name[skip..]) {
                        return true;
                    }
                }
                return false;
            }
            '?' => {
                if name_index >= name.len() {
                    return false;
                }
                name_index += 1;
                pattern_index += 1;
            }
            '[' => match parse_class(pattern, pattern_index) {
                Some((negated, items, next)) => {
                    if name_index >= name.len() {
                        return false;
                    }
                    let hit = items.contains(&name[name_index]);
                    if hit == negated {
                        return false;
                    }
                    name_index += 1;
                    pattern_index = next;
                }
                None => {
                    if name_index >= name.len() || name[name_index] != '[' {
                        return false;
                    }
                    name_index += 1;
                    pattern_index += 1;
                }
            },
            literal => {
                if name_index >= name.len() || name[name_index] != literal {
                    return false;
                }
                name_index += 1;
                pattern_index += 1;
            }
        }
    }
    name_index == name.len()
}

/// Parse `[...]` starting at `start`; `None` when it is not a valid class.
fn parse_class(pattern: &[char], start: usize) -> Option<(bool, Vec<char>, usize)> {
    let mut index = start + 1;
    let negated = matches!(pattern.get(index), Some('!' | '^'));
    if negated {
        index += 1;
    }
    let mut items = Vec::new();
    let first = index;
    while index < pattern.len() {
        if pattern[index] == ']' && index > first {
            return Some((negated, items, index + 1));
        }
        if index + 2 < pattern.len() && pattern[index + 1] == '-' && pattern[index + 2] != ']' {
            let (low, high) = (pattern[index], pattern[index + 2]);
            if low <= high {
                for value in low..=high {
                    items.push(value);
                }
                index += 3;
                continue;
            }
        }
        items.push(pattern[index]);
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `list_directory(file_name_glob=…)` is Python's `fnmatch.fnmatch`, corners
    /// included: `*` also matches the empty name, `**` is just `*` (not recursive), an
    /// unterminated `[` is a literal, an immediately-closing `]` belongs to the class,
    /// `[!…]` and `[^…]` both negate, and `{a,b}` is not an alternation.
    ///
    /// The `glob`/`globset` crates disagree on several of these (`**`, unterminated
    /// classes, `^`), so the matcher stays hand-written; this table pins the behaviour
    /// it must keep.
    #[test]
    fn glob_match_follows_fnmatches_corners() {
        const NAMES: &[&str] = &[
            "a.md",
            "b.md",
            "abc.md",
            ".hidden",
            "abc",
            "abc.tar.gz",
            "a*b",
            "f_o",
            "foo",
            "a",
            "",
            "[abc]",
            "a\\b",
            "A.md",
            "abc.txt",
            "[",
            "a[b",
            "{a,b}",
            "a[b]c",
            "]",
            "x]y",
            "]]",
        ];
        let cases: &[(&str, &[&str])] = &[
            ("*.md", &["a.md", "b.md", "abc.md", "A.md"]),
            ("?", &["a", "[", "]"]),
            ("abc*", &["abc.md", "abc", "abc.tar.gz", "abc.txt"]),
            (
                "[abc]*",
                &[
                    "a.md",
                    "b.md",
                    "abc.md",
                    "abc",
                    "abc.tar.gz",
                    "a*b",
                    "a",
                    "a\\b",
                    "abc.txt",
                    "a[b",
                    "a[b]c",
                ],
            ),
            (
                "[!abc]*",
                &[
                    ".hidden", "f_o", "foo", "[abc]", "A.md", "[", "{a,b}", "]", "x]y", "]]",
                ],
            ),
            (
                "[^abc]*",
                &[
                    ".hidden", "f_o", "foo", "[abc]", "A.md", "[", "{a,b}", "]", "x]y", "]]",
                ],
            ),
            ("[", &["["]),
            ("[]", &[]),
            ("[!]", &[]),
            ("[]]", &["]"]),
            ("a[b", &["a[b"]),
            ("{a,b}", &["{a,b}"]),
            (r"a\*b", &[r"a\b"]),
            ("*.tar.gz", &["abc.tar.gz"]),
            ("f*o", &["f_o", "foo"]),
            ("", &[""]),
            (
                "*.*",
                &[
                    "a.md",
                    "b.md",
                    "abc.md",
                    ".hidden",
                    "abc.tar.gz",
                    "A.md",
                    "abc.txt",
                ],
            ),
        ];
        for (pattern, expected) in cases {
            let hits: Vec<&str> = NAMES
                .iter()
                .copied()
                .filter(|name| glob_match(pattern, name))
                .collect();
            assert_eq!(&hits, expected, "pattern {pattern:?}");
        }
    }
}
