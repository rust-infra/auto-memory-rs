//! Configuration knobs that change observable behavior.
//!
//! Defaults mirror Basic Memory 0.23.2 (see `docs/reference.md`). Unknown keys in
//! a reference `config.json` are ignored so the same file can be loaded.
//!
//! The user-level file (`~/.config/auto-memory/config.json`) also carries the two
//! *locations* a command would otherwise have to be told: which index to open and
//! which project to default to. `specs/config-discovery-spec.md` owns the
//! precedence chains; this module owns loading the file and resolving them, so
//! the CLI and the hook cannot drift apart on "which index".

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Behavior-affecting configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Prefix generated permalinks with the project slug (reference default: `true`).
    pub permalinks_include_project: bool,
    /// Write derived `title`/`type`/`permalink` into files that lack frontmatter
    /// (reference default: `true`; this mutates user markdown).
    pub ensure_frontmatter_on_sync: bool,
    /// Never generate permalinks (reference default: `false`).
    pub disable_permalinks: bool,
    /// Watch and index local file changes (reference default: `true`).
    pub index_changes: bool,
    /// Rewrite permalinks when a file moves (reference default: `false`).
    pub update_permalinks_on_move: bool,
    /// Index to open when `--index` is not passed and `AUTO_MEMORY_INDEX` is unset.
    ///
    /// A leading `~` is expanded against `$HOME` when the value is used, not when
    /// the file is parsed, so the stored string stays what the user wrote.
    pub index: Option<PathBuf>,
    /// Project permalink to use when neither `--project` nor a project mapping
    /// file names one.
    ///
    /// The key is `default_project`, snake_case like every other key in this file
    /// (the reference's own `config.json` is snake_case too). The *harness mapping*
    /// files are the ones that use camelCase (`primaryProject`), because they follow
    /// their host's conventions.
    pub default_project: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        // Reference defaults from basic_memory/config_models.py (0.23.2).
        Self {
            permalinks_include_project: true,
            ensure_frontmatter_on_sync: true,
            disable_permalinks: false,
            index_changes: true,
            update_permalinks_on_move: false,
            index: None,
            default_project: None,
        }
    }
}

impl Config {
    /// Parse configuration from JSON, ignoring unknown reference keys.
    pub fn from_json_str(json: &str) -> Result<Self> {
        Ok(serde_json::from_str(json)?)
    }
}

// --- The user-level file ---

/// Where the user-level config file lives: `$XDG_CONFIG_HOME/auto-memory/config.json`,
/// defaulting to `~/.config/auto-memory/config.json`.
pub fn user_config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("auto-memory").join("config.json")
}

/// The default index path, used when nothing else names one.
pub fn default_index_path() -> PathBuf {
    std::env::var_os("HOME").map_or_else(
        || PathBuf::from(".local/share/auto-memory/memory.db"),
        |home| PathBuf::from(home).join(".local/share/auto-memory/memory.db"),
    )
}

/// The outcome of looking for the user config file.
///
/// Loading never fails, because the two callers want opposite policies: the hook
/// warns and continues (a broken config must not break a session), while the CLI
/// refuses to guess. Keeping the distinction here is what lets both read the same
/// chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserConfig {
    /// No file at this path.
    Absent {
        /// The path that was checked.
        path: PathBuf,
    },
    /// A parsed file.
    Loaded {
        /// Where it came from.
        path: PathBuf,
        /// The parsed values.
        config: Config,
    },
    /// A file that exists but cannot be used (unreadable, not JSON, not an object).
    Malformed {
        /// Where it came from.
        path: PathBuf,
        /// Why it was rejected.
        error: String,
    },
}

impl UserConfig {
    /// The path that was checked, whether or not anything was there.
    pub fn path(&self) -> &Path {
        match self {
            Self::Absent { path } | Self::Loaded { path, .. } | Self::Malformed { path, .. } => {
                path
            }
        }
    }

    /// The parsed values, or `None` when there is nothing usable.
    pub fn values(&self) -> Option<&Config> {
        match self {
            Self::Loaded { config, .. } => Some(config),
            Self::Absent { .. } | Self::Malformed { .. } => None,
        }
    }

    /// A one-line description for `doctor` and for warnings.
    pub fn describe(&self) -> String {
        match self {
            Self::Absent { path } => format!("{} (not present)", path.display()),
            Self::Loaded { path, .. } => format!("{} (loaded)", path.display()),
            Self::Malformed { path, error } => {
                format!("{} (unusable: {error})", path.display())
            }
        }
    }
}

/// Read the user config file; see [`UserConfig`] for the three outcomes.
pub fn load_user_config() -> UserConfig {
    load_user_config_at(&user_config_path())
}

/// [`load_user_config`] against an explicit path, so tests do not touch `$HOME`.
pub fn load_user_config_at(path: &Path) -> UserConfig {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return UserConfig::Absent {
                path: path.to_path_buf(),
            };
        }
        Err(error) => {
            return UserConfig::Malformed {
                path: path.to_path_buf(),
                error: error.to_string(),
            };
        }
    };
    match Config::from_json_str(&text) {
        Ok(config) => UserConfig::Loaded {
            path: path.to_path_buf(),
            config,
        },
        Err(error) => UserConfig::Malformed {
            path: path.to_path_buf(),
            error: error.to_string(),
        },
    }
}

// --- Resolution chains ---

/// Which step of a resolution chain produced a value.
///
/// Carried alongside the value rather than recomputed by the caller: `doctor`
/// prints it, and the CLI's errors name the step that failed. Without it, "why is
/// it using that index" is unanswerable from the outside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// An explicit command-line flag.
    Flag,
    /// An environment variable (deprecated, still honoured).
    Environment,
    /// The user config file at this path.
    Config(PathBuf),
    /// The project mapping file for the running harness.
    MappingFile,
    /// The `projects` row of the index that was opened.
    ProjectRow,
    /// The built-in default.
    Default,
}

impl std::fmt::Display for Origin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Flag => write!(formatter, "flag"),
            Self::Environment => write!(formatter, "environment"),
            Self::Config(path) => write!(formatter, "{}", path.display()),
            Self::MappingFile => write!(formatter, "project mapping file"),
            Self::ProjectRow => write!(formatter, "the index's project row"),
            Self::Default => write!(formatter, "default"),
        }
    }
}

/// A resolved value and the step that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved<T> {
    /// The value.
    pub value: T,
    /// Where it came from.
    pub origin: Origin,
}

/// Resolve the index: `--index` → `AUTO_MEMORY_INDEX` → the config file → the default.
///
/// The environment is an argument rather than a lookup so the chain can be tested
/// as data (`specs/config-discovery-spec.md` §8).
pub fn resolve_index(
    explicit: Option<PathBuf>,
    environment: Option<PathBuf>,
    user: &UserConfig,
) -> Resolved<PathBuf> {
    if let Some(value) = explicit {
        return Resolved {
            value: expand_tilde(&value),
            origin: Origin::Flag,
        };
    }
    if let Some(value) = environment {
        return Resolved {
            value: expand_tilde(&value),
            origin: Origin::Environment,
        };
    }
    if let Some(value) = user.values().and_then(|config| config.index.clone()) {
        return Resolved {
            value: expand_tilde(&value),
            origin: Origin::Config(user.path().to_path_buf()),
        };
    }
    Resolved {
        value: default_index_path(),
        origin: Origin::Default,
    }
}

/// Resolve the project permalink: `--project` → the harness mapping file → the
/// config file. `None` means nothing named a project, which is a state the hook
/// reports as a first-run nudge rather than an error.
pub fn resolve_project(
    explicit: Option<String>,
    mapped: Option<String>,
    user: &UserConfig,
) -> Option<Resolved<String>> {
    if let Some(value) = explicit {
        return Some(Resolved {
            value,
            origin: Origin::Flag,
        });
    }
    if let Some(value) = mapped {
        return Some(Resolved {
            value,
            origin: Origin::MappingFile,
        });
    }
    user.values()
        .and_then(|config| config.default_project.clone())
        .map(|value| Resolved {
            value,
            origin: Origin::Config(user.path().to_path_buf()),
        })
}

/// Expand a leading `~` against `$HOME`.
///
/// Only a leading `~` is special (a `~` elsewhere is a legal filename character),
/// and a missing `$HOME` leaves the path as written rather than guessing.
fn expand_tilde(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    let Some(rest) = text.strip_prefix('~') else {
        return path.to_path_buf();
    };
    if !(rest.is_empty() || rest.starts_with('/')) {
        return path.to_path_buf();
    }
    let Some(home) = std::env::var_os("HOME") else {
        return path.to_path_buf();
    };
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    PathBuf::from(home).join(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(path: &str, json: &str) -> UserConfig {
        UserConfig::Loaded {
            path: PathBuf::from(path),
            config: Config::from_json_str(json).expect("config"),
        }
    }

    fn absent() -> UserConfig {
        UserConfig::Absent {
            path: PathBuf::from("/nonexistent/config.json"),
        }
    }

    #[test]
    fn unknown_reference_keys_are_ignored_and_new_keys_parse() {
        let config = Config::from_json_str(
            r#"{"basicMemory": true, "index": "~/x/memory.db", "default_project": "oracle"}"#,
        )
        .expect("parse");
        assert_eq!(config.index, Some(PathBuf::from("~/x/memory.db")));
        assert_eq!(config.default_project.as_deref(), Some("oracle"));
        // Reference defaults survive a file that only sets the new keys.
        assert!(config.permalinks_include_project);
        assert!(!config.update_permalinks_on_move);
    }

    /// The keys are snake_case (this file and the reference's own `config.json`),
    /// unlike the harness mapping files' camelCase. A camelCase key here is silently
    /// ignored as an unknown reference key, so pin the difference.
    #[test]
    fn a_camel_case_key_is_ignored_rather_than_renamed() {
        let config = Config::from_json_str(r#"{"defaultProject": "oracle"}"#).expect("parse");
        assert_eq!(config.default_project, None);
    }

    #[test]
    fn the_index_chain_prefers_flag_then_environment_then_config_then_default() {
        let user = loaded("/cfg/config.json", r#"{"index": "/from/config.db"}"#);
        let resolved = resolve_index(
            Some(PathBuf::from("/from/flag.db")),
            Some(PathBuf::from("/from/env.db")),
            &user,
        );
        assert_eq!(resolved.value, PathBuf::from("/from/flag.db"));
        assert_eq!(resolved.origin, Origin::Flag);

        let resolved = resolve_index(None, Some(PathBuf::from("/from/env.db")), &user);
        assert_eq!(resolved.value, PathBuf::from("/from/env.db"));
        assert_eq!(resolved.origin, Origin::Environment);

        let resolved = resolve_index(None, None, &user);
        assert_eq!(resolved.value, PathBuf::from("/from/config.db"));
        assert_eq!(
            resolved.origin,
            Origin::Config(PathBuf::from("/cfg/config.json"))
        );
    }

    #[test]
    fn a_missing_or_unusable_config_falls_through_to_the_default() {
        for user in [
            absent(),
            UserConfig::Malformed {
                path: PathBuf::from("/cfg/config.json"),
                error: "expected value".to_owned(),
            },
        ] {
            let resolved = resolve_index(None, None, &user);
            assert_eq!(resolved.value, default_index_path());
            assert_eq!(resolved.origin, Origin::Default);
        }
    }

    #[test]
    fn the_project_chain_prefers_flag_then_mapping_then_config_then_nothing() {
        let user = loaded("/cfg/config.json", r#"{"default_project": "from-config"}"#);
        let resolved = resolve_project(
            Some("from-flag".to_owned()),
            Some("from-mapping".to_owned()),
            &user,
        )
        .expect("flag");
        assert_eq!(resolved.value, "from-flag");
        assert_eq!(resolved.origin, Origin::Flag);

        let resolved =
            resolve_project(None, Some("from-mapping".to_owned()), &user).expect("mapping");
        assert_eq!(resolved.value, "from-mapping");
        assert_eq!(resolved.origin, Origin::MappingFile);

        let resolved = resolve_project(None, None, &user).expect("config");
        assert_eq!(resolved.value, "from-config");
        assert_eq!(
            resolved.origin,
            Origin::Config(PathBuf::from("/cfg/config.json"))
        );

        assert!(resolve_project(None, None, &absent()).is_none());
    }

    #[test]
    fn a_leading_tilde_expands_against_home_and_nothing_else_does() {
        // The tests share the process environment, so assert the shape rather than
        // the value: an expanded path is absolute and no longer starts with `~`.
        let expanded = expand_tilde(Path::new("~/x/memory.db"));
        assert!(!expanded.to_string_lossy().starts_with('~'), "{expanded:?}");
        // `~` mid-path and `~user` are filenames, not home references.
        assert_eq!(
            expand_tilde(Path::new("/a/~/b.db")),
            PathBuf::from("/a/~/b.db")
        );
        assert_eq!(
            expand_tilde(Path::new("~someone/b.db")),
            PathBuf::from("~someone/b.db")
        );
    }

    #[test]
    fn a_malformed_file_is_reported_as_unusable_not_as_absent() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{ not json").expect("write");
        let user = load_user_config_at(&path);
        assert!(matches!(user, UserConfig::Malformed { .. }), "{user:?}");
        assert!(user.values().is_none());
        assert!(user.describe().contains("unusable"), "{}", user.describe());

        let missing = load_user_config_at(&dir.path().join("absent.json"));
        assert!(matches!(missing, UserConfig::Absent { .. }), "{missing:?}");
        assert!(missing.describe().contains("not present"));
    }
}
