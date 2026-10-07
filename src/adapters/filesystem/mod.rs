//! Declared placeholder for a filesystem adapter — it holds no code.
//!
//! Vault I/O is not an adapter boundary here: `indexing` is the only module that
//! touches the filesystem (`document`, `service`, `watcher`), and `storage` never
//! does. The directory exists because `specs/auto-memory-rs-spec.md` §6 declares it;
//! `docs/patterns.md` records why no trait was put behind it.
