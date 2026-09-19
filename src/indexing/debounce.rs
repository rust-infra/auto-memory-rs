//! Event debouncing for filesystem watching.
//!
//! Watchers emit several events per save (write, rename, metadata). The debouncer
//! coalesces them per path using last-write-wins semantics and releases a path
//! only when its newest event is older than the window.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Kind of filesystem change observed for a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// The path appeared.
    Created,
    /// The path content changed.
    Modified,
    /// The path disappeared.
    Removed,
}

/// One filesystem event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEvent {
    /// Project-relative, slash-separated path.
    pub path: String,
    /// Kind of change.
    pub kind: ChangeKind,
    /// When the event was observed.
    pub at: Instant,
}

/// Coalesces filesystem events into stable batches.
#[derive(Debug)]
pub struct Debouncer {
    window: Duration,
    pending: BTreeMap<String, FileEvent>,
}

impl Debouncer {
    /// Create a debouncer with the given quiet window (reference default: 1000 ms).
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            pending: BTreeMap::new(),
        }
    }

    /// The configured quiet window.
    pub fn window(&self) -> Duration {
        self.window
    }

    /// Number of paths currently waiting.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether nothing is waiting.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Record an event, replacing any earlier event for the same path.
    pub fn push(&mut self, event: FileEvent) {
        let replace = self
            .pending
            .get(&event.path)
            .is_none_or(|existing| existing.at <= event.at);
        if replace {
            self.pending.insert(event.path.clone(), event);
        }
    }

    /// Take every path whose newest event is at least one window old.
    pub fn drain_ready(&mut self, now: Instant) -> Vec<FileEvent> {
        let window = self.window;
        let ready: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, event)| now.duration_since(event.at) >= window)
            .map(|(path, _)| path.clone())
            .collect();
        ready
            .into_iter()
            .filter_map(|path| self.pending.remove(&path))
            .collect()
    }

    /// Take every pending event regardless of age.
    pub fn flush(&mut self) -> Vec<FileEvent> {
        std::mem::take(&mut self.pending).into_values().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(path: &str, kind: ChangeKind, at: Instant) -> FileEvent {
        FileEvent {
            path: path.to_owned(),
            kind,
            at,
        }
    }

    #[test]
    fn coalesces_repeated_events_per_path() {
        let start = Instant::now();
        let mut debouncer = Debouncer::new(Duration::from_millis(1000));
        debouncer.push(event("a.md", ChangeKind::Created, start));
        debouncer.push(event(
            "a.md",
            ChangeKind::Modified,
            start + Duration::from_millis(10),
        ));
        assert_eq!(debouncer.len(), 1);

        let ready = debouncer.drain_ready(start + Duration::from_millis(500));
        assert!(ready.is_empty(), "window has not elapsed");

        let ready = debouncer.drain_ready(start + Duration::from_millis(1500));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].kind, ChangeKind::Modified);
        assert!(debouncer.is_empty());
    }

    #[test]
    fn flush_returns_pending_events_in_path_order() {
        let start = Instant::now();
        let mut debouncer = Debouncer::new(Duration::from_millis(500));
        debouncer.push(event("b.md", ChangeKind::Created, start));
        debouncer.push(event("a.md", ChangeKind::Removed, start));
        let flushed = debouncer.flush();
        assert_eq!(flushed.len(), 2);
        assert_eq!(flushed[0].path, "a.md");
        assert_eq!(flushed[1].path, "b.md");
    }
}
