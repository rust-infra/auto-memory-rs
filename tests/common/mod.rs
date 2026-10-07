//! Golden-corpus helpers for Basic Memory behavior compatibility.
//!
//! Phase 1 scope: load golden files and canonicalize text so reference/Rust output
//! can be compared. JSON structural comparison lands with the parser milestone
//! (serde_json is not a dependency yet), so this module stays `std`-only.
// Shared by several golden test binaries; each uses a different subset.
#![allow(dead_code)]

use std::fs;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use auto_memory::domain::timeframe;
use auto_memory::indexing::{RebuildOptions, rebuild_vault};
use auto_memory::storage::Store;
use serde_json::Value;

/// Run an async operation on a test runtime without forcing an entire test to async.
pub fn block_on<F: Future>(future: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(future)),
        Err(_) => auto_memory::runtime::block_on(future).expect("test runtime"),
    }
}

/// Root of the repository (`CARGO_MANIFEST_DIR`).
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The hand-written fixture vault most tests copy into a temp directory.
pub fn fixtures_vault() -> PathBuf {
    repo_root().join("tests/fixtures/vault")
}

/// The post-sync vault the reference MCP captures were taken from.
pub fn golden_vault() -> PathBuf {
    repo_root().join("tests/golden/vault")
}

/// A scratch directory that deletes itself when it is dropped.
#[derive(Debug)]
pub struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    /// A fresh scratch directory whose name embeds `tag`.
    pub fn new(tag: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix(&format!("auto-memory-rs-{tag}-"))
            .tempdir()
            .expect("scratch dir");
        Self { dir }
    }

    /// The directory itself.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// A path inside the directory.
    pub fn join(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.dir.path().join(relative)
    }
}

/// Recursively copy a tree.
pub fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create dir");
    for entry in fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// Recursively copy a tree, preserving file mtimes.
///
/// The oracle harness copies with `shutil.copy2`, and `updated_at` falls back to the
/// file mtime, so cases that replay capture *order* (directory listings, recency) only
/// match the reference when the mtimes survive the copy.
pub fn copy_dir_with_mtimes(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create dir");
    for entry in fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir_with_mtimes(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
            if let Ok(modified) = fs::metadata(entry.path()).and_then(|meta| meta.modified())
                && let Ok(file) = fs::OpenOptions::new().write(true).open(&target)
            {
                let _ = file.set_modified(modified);
            }
        }
    }
}

/// Restore the file mtimes the golden corpus was captured with.
///
/// `entity.updated_at` falls back to the file mtime (`src/indexing/document.rs`), so
/// `list_directory`'s timestamp column and its `updated_desc` ordering — and
/// `recent_activity`'s recency list — all encode the mtimes of the tree the captures
/// were taken from. Git stores no mtimes, so a fresh clone sees its checkout time
/// instead and those three surfaces stop matching, no matter how often the corpus is
/// regenerated.
///
/// `tests/golden/index/graph-rows.json` recorded the capture-time values alongside the
/// reference's own ids, which makes them restorable rather than guessable. Files the
/// index skips (`notes/malformed-frontmatter.md`, `.obsidian/app.json`) have no
/// recorded value and take the oldest recorded instant: they are never rendered, but
/// leaving them at the copy time would hand any "newest file" computation — such as
/// `shift_mtimes` — a clock that is not the corpus's.
pub fn pin_fixture_mtimes(vault: &Path) {
    let graph = load_golden_json("index/graph-rows.json");
    let entities = graph["entities"].as_array().expect("entities");
    // The golden stores the value the way the index does — naive local wall-clock time —
    // so reading it back with the same parser keeps the local date, and with it the
    // rendered timestamp, independent of the machine's timezone.
    let recorded: Vec<(&str, timeframe::Instant)> = entities
        .iter()
        .filter_map(|entity| {
            let path = entity["file_path"].as_str()?;
            let stamp = entity["updated_at"].as_str()?;
            Some((path, timeframe::parse_frontmatter_timestamp(stamp).ok()?))
        })
        .collect();
    assert_eq!(
        recorded.len(),
        entities.len(),
        "every graph row must carry a file_path and an updated_at"
    );
    let oldest = recorded
        .iter()
        .map(|(_, instant)| *instant)
        .min()
        .expect("at least one recorded instant");

    let mut files = Vec::new();
    collect_files(vault, &mut files);
    for file in &files {
        let relative = file.strip_prefix(vault).expect("under the vault");
        let instant = recorded
            .iter()
            .find(|(path, _)| Path::new(path) == relative)
            .map_or(oldest, |(_, instant)| *instant);
        let modified =
            SystemTime::UNIX_EPOCH + Duration::from_micros(instant.timestamp_micros() as u64);
        let Ok(handle) = fs::OpenOptions::new().write(true).open(file) else {
            continue;
        };
        let _ = handle.set_modified(modified);
    }
}

/// Every file under `directory`, recursively.
pub fn collect_files(directory: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Copy the fixture vault into `to` and restore its capture-time mtimes, so surfaces
/// derived from `updated_at` are reproducible on any clone.
pub fn copy_fixture_vault(to: &Path) {
    copy_dir_with_mtimes(&fixtures_vault(), to);
    pin_fixture_mtimes(to);
}

/// A temp copy of the fixture vault with an empty in-memory index for project `oracle`.
pub fn fixture(tag: &str) -> (Scratch, PathBuf, Store, i64) {
    let dir = Scratch::new(tag);
    let vault = dir.join("vault");
    copy_dir(&fixtures_vault(), &vault);
    let (store, project_id) = block_on(async {
        let store = Store::open_in_memory().await.expect("store");
        let project_id = store
            .upsert_project("oracle", "oracle", &vault.to_string_lossy())
            .await
            .expect("project");
        (store, project_id)
    });
    (dir, vault, store, project_id)
}

/// A temp copy of the fixture vault, fully rebuilt into an in-memory index.
pub fn indexed_store(tag: &str) -> (Scratch, Store, i64) {
    let dir = Scratch::new(tag);
    let vault = dir.join("vault");
    copy_fixture_vault(&vault);
    let (mut store, project_id) = block_on(async {
        let store = Store::open_in_memory().await.expect("store");
        let project_id = store
            .upsert_project("oracle", "oracle", &vault.to_string_lossy())
            .await
            .expect("project");
        (store, project_id)
    });
    block_on(rebuild_vault(
        &mut store,
        project_id,
        &vault,
        &RebuildOptions::new("oracle"),
    ))
    .expect("rebuild");
    (dir, store, project_id)
}

/// Read a golden artifact by path relative to `tests/golden` as JSON.
pub fn load_golden_json(relative: &str) -> Value {
    serde_json::from_str(&load_golden(relative))
        .unwrap_or_else(|error| panic!("golden {relative} is not JSON: {error}"))
}

/// What one scripted MCP session produced.
#[derive(Debug)]
pub struct SessionOutput {
    /// Parsed stdout frames, in request order.
    pub frames: Vec<Value>,
    /// Raw stderr, which must never carry protocol frames (`specs/mcp-spec.md` §1).
    pub stderr: String,
}

/// One `auto-memory mcp` session over a vault, driven from a script of requests.
///
/// Every fixture project is called `oracle`, so that is the default; a test that needs
/// a different server shape (a model cache, a fixture embedding file) adds arguments
/// with [`Session::with_args`].
pub struct Session<'a> {
    vault: &'a Path,
    index: PathBuf,
    args: Vec<String>,
}

impl<'a> Session<'a> {
    /// A session over `vault`, indexing into `index`, pinned to project `oracle`.
    pub fn new(vault: &'a Path, index: impl Into<PathBuf>) -> Self {
        Self {
            vault,
            index: index.into(),
            args: ["--project", "oracle"].map(str::to_owned).to_vec(),
        }
    }

    /// Append extra `auto-memory mcp` arguments.
    #[must_use]
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Run the script and return the parsed frames plus stderr.
    pub fn run(&self, requests: &[Value]) -> SessionOutput {
        run_mcp_session(self.vault, &self.index, &self.args, requests)
    }
}

/// Run one `auto-memory mcp` session, feeding `requests` on stdin.
fn run_mcp_session(
    vault: &Path,
    index: &Path,
    extra_args: &[String],
    requests: &[Value],
) -> SessionOutput {
    let mut child = Command::new(env!("CARGO_BIN_EXE_auto-memory"))
        .args(["mcp", "--vault"])
        .arg(vault)
        .args(["--index"])
        .arg(index)
        .args(extra_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn auto-memory mcp");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        for request in requests {
            writeln!(stdin, "{request}").expect("write request");
        }
    }
    let output = child.wait_with_output().expect("mcp session");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "mcp server failed: {stderr}");
    let frames = String::from_utf8_lossy(&output.stdout)
        .lines()
        .enumerate()
        .map(|(number, line)| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("stdout line {number} is not JSON: {error}\n{line}"))
        })
        .collect();
    SessionOutput { frames, stderr }
}

/// The frame carrying `id`.
pub fn frame_of(frames: &[Value], id: u64) -> &Value {
    frames
        .iter()
        .find(|frame| frame["id"] == id)
        .unwrap_or_else(|| panic!("no frame for id {id}: {frames:?}"))
}

/// `content[0].text` of a frame — or of a golden case that wraps one under `frame`.
pub fn text_payload(frame: &Value) -> String {
    let frame = frame.get("frame").unwrap_or(frame);
    frame["result"]["content"][0]["text"]
        .as_str()
        .or_else(|| frame["content"][0]["text"].as_str())
        .unwrap_or_else(|| panic!("no text content: {frame}"))
        .to_owned()
}

/// Root of the generated golden corpus.
pub fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// Read a golden artifact by path relative to `tests/golden`.
///
/// Panics with a helpful message when the corpus has not been generated yet.
pub fn load_golden(relative: &str) -> String {
    let path = golden_dir().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "failed to read golden {}: {err}. Run `python3 tools/export_reference.py` \
             (requires the reference basic-memory CLI) to regenerate the corpus.",
            path.display()
        )
    })
}

fn is_hex(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

/// Replace RFC-4122 UUIDs with `<uuid>` (stable golden placeholder).
pub fn canonicalize_uuids(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        // 8-4-4-4-12 hex with dashes.
        let candidate = |start: usize| -> Option<usize> {
            let groups = [8usize, 4, 4, 4, 12];
            let mut pos = start;
            for (idx, len) in groups.iter().enumerate() {
                if idx > 0 {
                    if bytes.get(pos) != Some(&b'-') {
                        return None;
                    }
                    pos += 1;
                }
                for _ in 0..*len {
                    if !bytes.get(pos).is_some_and(|b| is_hex(*b)) {
                        return None;
                    }
                    pos += 1;
                }
            }
            Some(pos)
        };
        match candidate(i) {
            Some(end) => {
                out.push_str("<uuid>");
                i = end;
            }
            None => {
                let ch = input[i..].chars().next().expect("char boundary");
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// Replace ISO-8601 timestamps (`YYYY-MM-DD[ T]HH:MM:SS[.fff][Z|±HH:MM]`) with `<timestamp>`.
pub fn canonicalize_timestamps(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < chars.len() {
        if let Some(end) = match_timestamp(&chars, i) {
            out.push_str("<timestamp>");
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn match_timestamp(chars: &[char], start: usize) -> Option<usize> {
    let digit = |idx: usize, count: usize| -> Option<usize> {
        let mut pos = idx;
        for _ in 0..count {
            if !chars.get(pos).is_some_and(|c| c.is_ascii_digit()) {
                return None;
            }
            pos += 1;
        }
        Some(pos)
    };
    let mut pos = digit(start, 4)?;
    if chars.get(pos) != Some(&'-') {
        return None;
    }
    pos = digit(pos + 1, 2)?;
    if chars.get(pos) != Some(&'-') {
        return None;
    }
    pos = digit(pos + 1, 2)?;
    match chars.get(pos) {
        Some('T') | Some(' ') => pos += 1,
        _ => return None,
    }
    pos = digit(pos, 2)?;
    if chars.get(pos) != Some(&':') {
        return None;
    }
    pos = digit(pos + 1, 2)?;
    if chars.get(pos) != Some(&':') {
        return None;
    }
    pos = digit(pos + 1, 2)?;
    if chars.get(pos) == Some(&'.') {
        pos += 1;
        let frac_start = pos;
        while chars.get(pos).is_some_and(|c| c.is_ascii_digit()) {
            pos += 1;
        }
        if pos == frac_start {
            return None;
        }
    }
    match chars.get(pos) {
        Some('Z') => pos += 1,
        Some('+') | Some('-') => {
            pos = digit(pos + 1, 2)?;
            if chars.get(pos) == Some(&':') {
                pos += 1;
            }
            pos = digit(pos, 2)?;
        }
        _ => {}
    }
    Some(pos)
}

/// Canonicalize reference output for text comparison.
pub fn canonicalize_text(input: &str) -> String {
    let normalized = input.replace("\r\n", "\n");
    canonicalize_timestamps(&canonicalize_uuids(&normalized))
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_owned()
}

/// Bridge this port's deliberately renamed surface back to the reference's wording.
///
/// `tests/golden/**` is a faithful capture of `basic-memory` 0.23.2 and stays
/// byte-for-byte reproducible from `tools/export_reference.py`; this port renames the
/// user-visible surface (`Auto Memory`, and its own binary in the guidance text), so
/// comparisons map that surface back before asserting equality. Each pair is a whole
/// phrase rather than a word swap, so the mapping stays unambiguous; the one bare
/// identifier is the diagnostics tool name, which clients call by name. Every other
/// byte of a payload still has to match the reference.
const RENAMED_SURFACE: &[(&str, &str)] = &[
    ("# Auto Memory Diagnostics", "# Basic Memory Diagnostics"),
    (
        "Create a new Auto Memory project.",
        "Create a new Basic Memory project.",
    ),
    (
        "Delete an Auto Memory project.",
        "Delete a Basic Memory project.",
    ),
    ("Auto Memory keeps notes", "Basic Memory keeps notes"),
    (
        "Restart the Auto Memory server/process",
        "Restart the Basic Memory server/process",
    ),
    ("auto_memory_diagnostics", "basic_memory_diagnostics"),
    ("`auto-memory status`", "`basic-memory status`"),
    (
        "run `auto-memory reindex`",
        "run `basic-memory db reindex --search`",
    ),
    // The constrained-server refusals name the CLI that can do the job. Both commands
    // now exist here, so this is a rename like the others rather than a divergence.
    ("`auto-memory project add", "`basic-memory project add"),
    (
        "`auto-memory project remove",
        "`basic-memory project remove",
    ),
];

/// Map the renamed surface back to the reference wording (see [`RENAMED_SURFACE`]).
pub fn canonicalize_renames(input: &str) -> String {
    let mut out = input.to_owned();
    for (ours, reference) in RENAMED_SURFACE {
        out = out.replace(ours, reference);
    }
    out
}
