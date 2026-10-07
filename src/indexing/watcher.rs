//! OS filesystem watching: `notify` events → debounce → index updates.
//!
//! Mirrors the reference watcher (`index/local_watch.py` + `watch_service.py`): the
//! OS events are coalesced per path with the configured `index_delay` (1000 ms),
//! ignored paths are dropped before they reach the indexer, and a delete followed by
//! a create of the same content inside the window is treated as a **move** so the
//! entity (and its permalink, since `update_permalinks_on_move=false`) survives.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecursiveMode, Watcher, event::ModifyKind};
use serde::Serialize;

use crate::error::Result;
use crate::indexing::debounce::{ChangeKind, Debouncer, FileEvent};
use crate::indexing::service::{IndexOutcome, IndexService};

/// Reference `index_delay` default.
pub const DEFAULT_WATCH_WINDOW: Duration = Duration::from_millis(1000);

/// How often the async loop wakes to drain the debouncer when no event arrives.
///
/// This replaces the blocking loop's `recv_timeout(100ms)`: the quiet window is
/// measured per path by the [`Debouncer`], so the tick only bounds how late a ready
/// path can be released.
pub const WATCH_TICK: Duration = Duration::from_millis(100);

/// Summary of one applied watch batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct WatchReport {
    /// Paths written to the index (created or changed).
    pub indexed: usize,
    /// Paths whose stored checksum already matched.
    pub unchanged: usize,
    /// Paths recognized as a move of an existing entity.
    pub moved: usize,
    /// Paths removed from the index.
    pub removed: usize,
    /// Paths skipped because they are unreadable or malformed.
    pub skipped: usize,
}

impl WatchReport {
    /// Whether the batch changed nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Directories and file patterns the reference never indexes.
pub const DEFAULT_IGNORE_PATTERNS: &[&str] = &[
    ".*",
    "*.db",
    "*.db-shm",
    "*.db-wal",
    "config.json",
    ".git",
    ".svn",
    "__pycache__",
    "*.pyc",
    "*.pyo",
    "*.pyd",
    ".pytest_cache",
    ".coverage",
    "*.egg-info",
    ".tox",
    ".mypy_cache",
    ".ruff_cache",
    ".venv",
    "venv",
    "env",
    ".env",
    "node_modules",
    "build",
    "dist",
    ".cache",
    ".idea",
    ".vscode",
    ".DS_Store",
    "Thumbs.db",
    "desktop.ini",
    ".obsidian",
    "*.tmp",
    "*.swp",
    "*.swo",
    "*~",
];

/// Ignore rules for watched paths (reference `DEFAULT_IGNORE_PATTERNS` + `.bmignore`).
#[derive(Debug, Clone)]
pub struct IgnoreRules {
    patterns: Vec<String>,
}

impl Default for IgnoreRules {
    fn default() -> Self {
        Self {
            patterns: DEFAULT_IGNORE_PATTERNS
                .iter()
                .map(|pattern| (*pattern).to_owned())
                .collect(),
        }
    }
}

impl IgnoreRules {
    /// Reference defaults plus any `.bmignore` entries and extra patterns.
    pub fn load(root: &Path, extra_patterns: &[String]) -> Self {
        let mut rules = Self::default();
        rules.patterns.extend(extra_patterns.iter().cloned());
        rules.patterns.extend(read_bmignore(root));
        rules
    }

    /// Whether a project-relative path is ignored.
    ///
    /// Mirrors `should_ignore_path`: directory patterns match any path part, glob
    /// patterns match each part and the full relative path.
    pub fn should_ignore(&self, relative: &str) -> bool {
        if relative.is_empty() {
            return false;
        }
        let parts: Vec<&str> = relative
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        for pattern in &self.patterns {
            let pattern = pattern.trim();
            if pattern.is_empty() || pattern.starts_with('#') {
                continue;
            }
            let pattern = pattern.strip_prefix('/').unwrap_or(pattern);
            if let Some(directory) = pattern.strip_suffix('/') {
                if parts.contains(&directory) {
                    return true;
                }
                continue;
            }
            if parts.iter().any(|part| glob_match(pattern, part)) {
                return true;
            }
            if glob_match(pattern, relative) {
                return true;
            }
        }
        false
    }
}

/// Read `.bmignore` patterns from the vault root (one pattern per line).
fn read_bmignore(root: &Path) -> Vec<String> {
    let Ok(contents) = std::fs::read_to_string(root.join(".bmignore")) else {
        return Vec::new();
    };
    contents
        .lines()
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// Shell-style glob match (`*`, `?`, `[...]`) over one string.
pub fn glob_match(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();
    glob_match_at(&pattern, &value, 0, 0)
}

fn glob_match_at(pattern: &[char], value: &[char], mut p: usize, mut v: usize) -> bool {
    while p < pattern.len() {
        match pattern[p] {
            '*' => {
                while p + 1 < pattern.len() && pattern[p + 1] == '*' {
                    p += 1;
                }
                if p + 1 == pattern.len() {
                    return true;
                }
                for start in v..=value.len() {
                    if glob_match_at(pattern, value, p + 1, start) {
                        return true;
                    }
                }
                return false;
            }
            '?' => {
                if v >= value.len() {
                    return false;
                }
                p += 1;
                v += 1;
            }
            '[' => {
                let Some(end) = pattern[p + 1..].iter().position(|c| *c == ']') else {
                    return false;
                };
                let set = &pattern[p + 1..p + 1 + end];
                let negated = set.first() == Some(&'!') || set.first() == Some(&'^');
                let set = if negated { &set[1..] } else { set };
                let Some(candidate) = value.get(v) else {
                    return false;
                };
                if set.contains(candidate) == negated {
                    return false;
                }
                p += end + 2;
                v += 1;
            }
            literal => {
                if value.get(v) != Some(&literal) {
                    return false;
                }
                p += 1;
                v += 1;
            }
        }
    }
    v == value.len()
}

/// One watched vault.
pub struct VaultWatcher<'a> {
    service: IndexService<'a>,
    root: PathBuf,
    debouncer: Debouncer,
    ignore: IgnoreRules,
}

impl<'a> VaultWatcher<'a> {
    /// Wrap an indexing service for `root` with the reference debounce window.
    ///
    /// `root` is canonicalized first; the watcher compares every OS event against this
    /// root, so it has to be the form the backend reports.
    pub fn new(service: IndexService<'a>, root: impl Into<PathBuf>) -> Self {
        let root = resolve_watch_root(root.into());
        let ignore = IgnoreRules::load(&root, &[]);
        Self {
            service,
            root,
            debouncer: Debouncer::new(DEFAULT_WATCH_WINDOW),
            ignore,
        }
    }

    /// Override the debounce window (tests use a short one).
    pub fn with_window(mut self, window: Duration) -> Self {
        self.debouncer = Debouncer::new(window);
        self
    }

    /// The configured debounce window.
    pub fn window(&self) -> Duration {
        self.debouncer.window()
    }

    /// Record one event for a project-relative path.
    ///
    /// Paths that are not markdown or that match the ignore rules are dropped,
    /// matching the reference `should_ignore_path` filter.
    pub fn record(&mut self, relative: &str, kind: ChangeKind) {
        self.record_at(relative, kind, Instant::now());
    }

    /// Record one event with an explicit timestamp (tests).
    pub fn record_at(&mut self, relative: &str, kind: ChangeKind, at: Instant) {
        let relative = normalize_relative(relative);
        if relative.is_empty() || self.ignore.should_ignore(&relative) {
            // The usual "why did my file not get indexed?" answer; `debug` keeps the
            // default view free of the `.obsidian/` churn Obsidian generates.
            tracing::debug!(path = %relative, "watch: ignored path");
            return;
        }
        if !relative.to_ascii_lowercase().ends_with(".md") {
            tracing::debug!(path = %relative, "watch: not a markdown file");
            return;
        }
        self.debouncer.push(FileEvent {
            path: relative,
            kind,
            at,
        });
    }

    /// Number of paths waiting for their quiet window.
    pub fn pending(&self) -> usize {
        self.debouncer.len()
    }

    /// Apply every event whose quiet window elapsed.
    pub async fn poll(&mut self, now: Instant) -> Result<WatchReport> {
        let ready = self.debouncer.drain_ready(now);
        self.apply(ready).await
    }

    /// Apply everything still pending (shutdown path).
    pub async fn flush(&mut self) -> Result<WatchReport> {
        let pending = self.debouncer.flush();
        self.apply(pending).await
    }

    async fn apply(&mut self, events: Vec<FileEvent>) -> Result<WatchReport> {
        let mut report = WatchReport::default();
        // Paths that vanished inside this window stay in the index until the batch is
        // finished: a rename arrives as remove + create, and the delete/create pair
        // must become a move (the entity row and its permalink survive) rather than a
        // delete followed by a fresh insert.
        let mut missing: BTreeSet<String> = BTreeSet::new();
        for event in &events {
            if event.kind == ChangeKind::Removed || !self.root.join(&event.path).exists() {
                missing.insert(event.path.clone());
            }
        }
        let mut candidates: BTreeMap<String, String> = BTreeMap::new();
        for path in &missing {
            if let Some(checksum) = self.service.entity_checksum(path).await? {
                candidates.insert(checksum, path.clone());
            }
        }

        for event in events
            .iter()
            .filter(|event| event.kind != ChangeKind::Removed)
        {
            if missing.contains(&event.path) || !self.root.join(&event.path).exists() {
                continue;
            }
            let checksum = self.service.file_checksum(&event.path)?;
            if let Some(from) = checksum
                .as_ref()
                .and_then(|checksum| candidates.remove(checksum))
                .filter(|from| *from != event.path)
            {
                missing.remove(&from);
                self.service.move_file(&from, &event.path).await?;
                report.moved += 1;
                continue;
            }
            match self.service.index_file(&event.path).await? {
                IndexOutcome::Indexed => report.indexed += 1,
                IndexOutcome::Unchanged => report.unchanged += 1,
                IndexOutcome::SkippedMalformed | IndexOutcome::Missing => report.skipped += 1,
            }
        }

        for path in missing {
            if self.service.entity_checksum(&path).await?.is_some() {
                self.service.remove_file(&path).await?;
                report.removed += 1;
            }
        }
        Ok(report)
    }
}

/// Resolve the root every OS event is measured against.
///
/// `map_notify_event` maps an event to a project-relative path with
/// `path.strip_prefix(root)`, and drops the event when that fails — so the root and the
/// paths the backend reports have to be the *same* spelling of the same directory.
/// They are not, by default: FSEvents (macOS) reports the canonical path of every event,
/// so a root that is itself non-canonical never matches. On macOS that is the common
/// case rather than a corner — `/var` is a symlink to `/private/var`, so every vault
/// under `TMPDIR` (`/var/folders/…`) sees `/private/var/folders/…` come back and indexes
/// nothing at all. A relative root and a symlinked vault directory (`~/notes` →
/// `/Volumes/notes`) break the same way.
///
/// Canonicalizing once, up front, makes the two agree: the same resolved path is handed
/// to `handle.watch`, so inotify (Linux) — which echoes back the path it was registered
/// with rather than a canonical one — agrees as well.
///
/// A root that cannot be resolved (it does not exist yet, or is not readable) is kept as
/// given: `watch` fails on it a moment later with the real error, which beats failing
/// here with a confusing one.
fn resolve_watch_root(root: PathBuf) -> PathBuf {
    match root.canonicalize() {
        Ok(canonical) => canonical,
        Err(error) => {
            tracing::debug!(
                root = %root.display(),
                %error,
                "watch: root is not resolvable, watching it as given"
            );
            root
        }
    }
}

/// Normalize a path into the project-relative slash form used by the index.
pub fn normalize_relative(path: &str) -> String {
    path.trim_start_matches("./")
        .replace('\\', "/")
        .trim_matches('/')
        .to_owned()
}

/// Map one `notify` event into `(relative path, change kind)` pairs.
pub fn map_notify_event(root: &Path, event: &Event) -> Vec<(String, ChangeKind)> {
    let kinds: Vec<ChangeKind> = match event.kind {
        EventKind::Create(_) => vec![ChangeKind::Created],
        EventKind::Modify(ModifyKind::Name(notify::event::RenameMode::Both)) => {
            vec![ChangeKind::Removed, ChangeKind::Created]
        }
        EventKind::Modify(ModifyKind::Name(notify::event::RenameMode::From)) => {
            vec![ChangeKind::Removed]
        }
        EventKind::Modify(ModifyKind::Name(notify::event::RenameMode::To)) => {
            vec![ChangeKind::Created]
        }
        EventKind::Modify(_) => vec![ChangeKind::Modified],
        EventKind::Remove(_) => vec![ChangeKind::Removed],
        _ => Vec::new(),
    };
    if kinds.is_empty() {
        return Vec::new();
    }

    let mut mapped = Vec::new();
    let rename_pair = kinds.len() == 2 && event.paths.len() == 2;
    for (index, path) in event.paths.iter().enumerate() {
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let relative = normalize_relative(&relative.to_string_lossy());
        if relative.is_empty() {
            continue;
        }
        if rename_pair {
            // A rename carries `[from, to]`; the first path was removed, the second
            // created.
            mapped.push((relative, kinds[index]));
        } else {
            for kind in &kinds {
                mapped.push((relative.clone(), *kind));
            }
        }
    }
    mapped
}

/// Watch `root` until `shutdown` resolves, indexing debounced changes.
///
/// Returns the number of batches that changed something. The loop mirrors the
/// reference watch service: raw OS events feed the debouncer, and each quiet window
/// triggers one incremental sync. Compared with the blocking loop this replaces, the
/// event channel is a tokio channel fed by the `notify` callback, the quiet window is
/// a timer arm instead of a `recv_timeout` poll, and the loop ends on a future rather
/// than a polled flag — which is what makes a graceful stop (Ctrl-C, then flush the
/// pending window) possible without racing the signal.
///
/// This installs the watch and runs the loop; the caller's own catch-up pass
/// (`IndexService::reconcile`, as `watch` runs before calling in) is not this function's
/// business, and the two are not ordered against each other here.
///
/// Indexing and SQLite are awaited directly; CPU-heavy work inside the index path can
/// still move to the blocking pool. The loop runs on the multi-thread runtime built by
/// [`crate::runtime::executor`].
pub async fn watch_vault(
    mut watcher: VaultWatcher<'_>,
    shutdown: impl Future<Output = ()>,
) -> Result<usize> {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut handle = notify::recommended_watcher(move |event| {
        let _ = sender.send(event);
    })
    .map_err(|error| std::io::Error::other(error.to_string()))?;
    handle
        .watch(watcher.root.as_path(), RecursiveMode::Recursive)
        .map_err(|error| std::io::Error::other(error.to_string()))?;

    let root = watcher.root.clone();
    tokio::pin!(shutdown);
    let mut ticker = tokio::time::interval(WATCH_TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut batches = 0;
    loop {
        let wake = tokio::select! {
            event = receiver.recv() => WatchWake::Event(event),
            _ = ticker.tick() => WatchWake::Tick,
            () = &mut shutdown => WatchWake::Shutdown,
        };
        match wake {
            WatchWake::Event(Some(Ok(event))) => {
                for (path, kind) in map_notify_event(&root, &event) {
                    tracing::debug!(path = %path, kind = ?kind, "filesystem event");
                    watcher.record(&path, kind);
                }
            }
            // A `notify` error is transient; the reference swallows it too. Worth a
            // line at `debug` so a missed index update can be traced back to it.
            WatchWake::Event(Some(Err(error))) => {
                tracing::debug!(%error, "watch: notify reported an error");
            }
            // The channel closes only if the watcher handle goes away, at which point
            // there is nothing left to watch (and `recv` would spin).
            WatchWake::Event(None) => break,
            WatchWake::Shutdown => break,
            WatchWake::Tick => {}
        }
        let report = watcher.poll(Instant::now()).await?;
        if !report.is_empty() {
            batches += 1;
            log_watch_report("watch batch", &report);
        }
    }
    let report = watcher.flush().await?;
    if !report.is_empty() {
        batches += 1;
        log_watch_report("final flush", &report);
    }
    Ok(batches)
}

/// Report one applied batch on stderr.
///
/// This is a running `watch` daemon's only sign of life: the CLI prints the batch
/// count on stdout when it stops, but nothing before that. Empty batches (a tick with
/// no ready path) are the normal case and stay at `debug`, so the default view shows
/// exactly the windows that changed something.
pub fn log_watch_report(label: &str, report: &WatchReport) {
    if report.is_empty() {
        tracing::debug!(label, "watch batch: nothing to apply");
        return;
    }
    tracing::info!(
        label,
        indexed = report.indexed,
        moved = report.moved,
        removed = report.removed,
        unchanged = report.unchanged,
        "watch batch applied"
    );
}

/// Which arm of the watch loop's `select!` woke it.
enum WatchWake {
    /// An OS event (or a closed channel).
    Event(Option<notify::Result<Event>>),
    /// The quiet-window timer elapsed.
    Tick,
    /// The caller asked the loop to stop.
    Shutdown,
}

/// A shutdown future that resolves once `predicate` reports true.
///
/// [`watch_vault`] used to take an `Fn() -> bool` flag; this keeps that calling
/// convention (tests and examples flip an `AtomicBool`) without putting a polling
/// loop inside the watcher itself.
pub async fn shutdown_when(predicate: impl Fn() -> bool) {
    while !predicate() {
        tokio::time::sleep(WATCH_TICK).await;
    }
}

/// Collect events for one quiet window and apply the resulting batch.
///
/// This is the `watch --once` primitive: useful for scripts or for a single manual
/// sync, without running the loop forever.
pub async fn watch_once(mut watcher: VaultWatcher<'_>, window: Duration) -> Result<WatchReport> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut handle = notify::recommended_watcher(move |event| {
        let _ = sender.send(event);
    })
    .map_err(|error| std::io::Error::other(error.to_string()))?;
    handle
        .watch(watcher.root.as_path(), RecursiveMode::Recursive)
        .map_err(|error| std::io::Error::other(error.to_string()))?;

    let deadline = Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match receiver.recv_timeout(remaining) {
            Ok(Ok(event)) => {
                for (path, kind) in map_notify_event(&watcher.root, &event) {
                    watcher.record(&path, kind);
                }
            }
            Ok(Err(_)) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    watcher.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, ModifyKind as Kind, RenameMode};

    fn rule_set() -> IgnoreRules {
        IgnoreRules::default()
    }

    #[test]
    fn ignore_rules_match_the_reference_defaults() {
        let rules = rule_set();
        for ignored in [
            ".obsidian/app.json",
            ".hidden.md",
            "notes/.draft.md",
            "node_modules/pkg/readme.md",
            "notes/archive~",
            "notes/scratch.tmp",
            "config.json",
        ] {
            assert!(rules.should_ignore(ignored), "{ignored} must be ignored");
        }
        for kept in ["notes/simple.md", "projects/alpha.md", "README.md"] {
            assert!(!rules.should_ignore(kept), "{kept} must be indexed");
        }
    }

    #[test]
    fn bmignore_patterns_are_loaded_from_the_vault() {
        let dir = std::env::temp_dir().join(format!("am-ignore-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join(".bmignore"), "# comment\ndrafts/\n*.bak\n").expect("write");
        let rules = IgnoreRules::load(&dir, &[]);
        assert!(rules.should_ignore("drafts/idea.md"));
        assert!(rules.should_ignore("notes/old.bak"));
        assert!(!rules.should_ignore("notes/new.md"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn notify_events_map_to_relative_paths_and_kinds() {
        let root = PathBuf::from("/tmp/vault");
        let mut create = Event::new(EventKind::Create(CreateKind::File));
        create.paths = vec![root.join("notes/new.md")];
        assert_eq!(
            map_notify_event(&root, &create),
            vec![("notes/new.md".to_owned(), ChangeKind::Created)]
        );

        let mut rename = Event::new(EventKind::Modify(Kind::Name(RenameMode::Both)));
        rename.paths = vec![root.join("notes/old.md"), root.join("notes/new.md")];
        assert_eq!(
            map_notify_event(&root, &rename),
            vec![
                ("notes/old.md".to_owned(), ChangeKind::Removed),
                ("notes/new.md".to_owned(), ChangeKind::Created),
            ]
        );

        let mut outside = Event::new(EventKind::Remove(notify::event::RemoveKind::File));
        outside.paths = vec![PathBuf::from("/elsewhere/x.md")];
        assert!(map_notify_event(&root, &outside).is_empty());

        let mut access = Event::new(EventKind::Access(notify::event::AccessKind::Read));
        access.paths = vec![root.join("notes/new.md")];
        assert!(map_notify_event(&root, &access).is_empty());
    }

    /// The root has to be the spelling the backend reports events under, or
    /// `map_notify_event` drops all of them (see [`resolve_watch_root`]).
    #[test]
    fn the_watch_root_is_resolved_before_events_are_measured_against_it() {
        let dir = std::env::temp_dir().join(format!("am-watch-root-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        // Asserted against `canonicalize` rather than a literal, because whether this
        // path has a non-canonical spelling is the platform's business: macOS answers
        // `/private/var/…` for `/var/…`, Linux answers the same path it was given.
        assert_eq!(
            resolve_watch_root(dir.clone()),
            dir.canonicalize().expect("canonicalize")
        );
        // An unresolvable root is kept as given, so `watch` reports the real error.
        let missing = dir.join("does-not-exist");
        assert_eq!(resolve_watch_root(missing.clone()), missing);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn glob_match_handles_stars_questions_and_classes() {
        assert!(glob_match("*.md", "note.md"));
        assert!(!glob_match("*.md", "note.txt"));
        assert!(glob_match(".?", ".a"));
        assert!(glob_match("[ab].md", "a.md"));
        assert!(!glob_match("[ab].md", "c.md"));
        assert!(glob_match("notes/**/deep.md", "notes/**/deep.md"));
        assert!(glob_match("*", "anything"));
    }
}
