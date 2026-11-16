//! The user's configuration file: which agent backends there are and how each one's adapter
//! is started.
//!
//! The file is TOML. Every entry of the `backends` table is one backend, named by its key:
//!
//! ```toml
//! default = "claude-main"            # the backend new sessions run on
//!
//! [backends.claude-main]
//! command = "claude-agent-acp"       # the ACP adapter; a bare name is looked up on PATH
//! env = { CLAUDE_CONFIG_DIR = "~/.claude-main" }
//!
//! [backends.claude-work]
//! command = "claude-agent-acp"
//! args = []                          # the adapter's arguments; none by default
//! env = { CLAUDE_CONFIG_DIR = "~/.claude-work" }
//! thinking = "summarized"            # or "omitted"; summarized by default
//! ```
//!
//! A top-level `worktree_path` sets the global default for where new worktrees are created:
//!
//! ```toml
//! worktree_path = "~/worktrees/{repo}/{branch}"
//! ```
//!
//! An unknown `{placeholder}` in it stops the start with an error naming the file.
//!
//! A top-level `link_opener` sets the global command a clicked link is opened with; it is
//! split on whitespace and the link is appended as the last argument. Left out, links open
//! with the system's default handler (`open` on macOS, `xdg-open` on Linux); a value without
//! a program stops the start the same way:
//!
//! ```toml
//! link_opener = "firefox -P work"
//! ```
//!
//! A name must be non-empty and contain no `/` or `\`. `command` is required; `default` may
//! be left out when only one backend is configured.
//!
//! Backends are resolved on top of a process environment, such as the login shell's: its
//! variables come before a backend's `env`, which wins over them, and a bare command is looked
//! up on its `PATH` (else this process's). [`resolve_backends`] stops at the first missing
//! command; [`resolution`] resolves each backend on its own, so a missing command fails only
//! the sessions of its backend. [`resolve_at_start`] picks between them: whenever the login
//! shell is asked for the environment, resolution waits for its answer, even if it came at once.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::{env, fs, io};

use directories::BaseDirs;
use partitur_backend_acp::{
    ADAPTER_COMMAND, ADAPTER_PACKAGE, AcpConfig, Resolution, ThinkingDisplay,
};
use partitur_core::{BackendId, BackendInfo, Level, ProcessEnv, SettingError, SettingsLayer};
use serde::Deserialize;

/// The name of the one backend there is without a configuration file.
const BUILTIN_BACKEND: &str = "claude";

/// Everything the configuration file sets.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The backend new sessions run on; may be left out when only one is configured.
    pub default: Option<BackendId>,
    /// The configured backends, by name.
    #[serde(default)]
    pub backends: BTreeMap<BackendId, BackendConfig>,
    /// The global pattern for where new worktrees are created; see [`global_settings`].
    #[serde(default)]
    pub worktree_path: Option<String>,
    /// The global command clicked links are opened with; see [`global_settings`].
    #[serde(default)]
    pub link_opener: Option<String>,
}

/// How one backend's agent adapter is started.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendConfig {
    /// The adapter program; a bare name is looked up on `PATH`.
    pub command: PathBuf,
    /// The adapter's arguments; none by default.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables for the adapter; a value starting with `~` or `~/` has
    /// that part replaced by the home folder.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// How the agent's reasoning is reported; summarized by default.
    #[serde(default, with = "ThinkingDisplayDef")]
    pub thinking: ThinkingDisplay,
}

/// The file's spelling of [`ThinkingDisplay`]: `"summarized"` or `"omitted"`.
#[derive(Deserialize)]
#[serde(remote = "ThinkingDisplay", rename_all = "lowercase")]
enum ThinkingDisplayDef {
    Summarized,
    Omitted,
}

/// The configured backends, ready to start.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// How to start each backend's adapter.
    pub backends: HashMap<BackendId, AcpConfig>,
    /// The backends as the core and the UI show them, sorted by id.
    pub infos: Vec<BackendInfo>,
    /// The backend new sessions run on; one of the keys of `backends`.
    pub default: BackendId,
}

/// Why the configuration could not be used.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The file exists but could not be read.
    #[error("failed to read the config file {}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The read error.
        source: io::Error,
    },
    /// The file is not valid TOML or does not match the expected keys.
    #[error("failed to parse the config file {}", path.display())]
    Parse {
        /// The file.
        path: PathBuf,
        /// The parse error, with the line and column.
        source: toml::de::Error,
    },
    /// A backend's adapter command is a bare name found in no folder of `PATH`.
    #[error("the adapter command {} of backend {backend} is not on PATH", command.display())]
    CommandNotFound {
        /// The backend whose command is missing.
        backend: BackendId,
        /// The configured command.
        command: PathBuf,
    },
    /// The file's `backends` table is missing or empty.
    #[error("the config file {} configures no backend", path.display())]
    NoBackends {
        /// The file.
        path: PathBuf,
    },
    /// The file's `default` names a backend it does not configure.
    #[error(
        "the config file {} names {default} as the default backend, which it does not configure",
        path.display()
    )]
    UnknownDefault {
        /// The file.
        path: PathBuf,
        /// The named default.
        default: BackendId,
    },
    /// A setting of the file, `worktree_path` or `link_opener`, has an invalid value.
    #[error("invalid {key} in the config file {}", path.display())]
    Setting {
        /// The file.
        path: PathBuf,
        /// The setting's key.
        key: &'static str,
        /// Why the value is invalid.
        source: SettingError,
    },
    /// The file configures more than one backend and names none as the default.
    #[error(
        "the config file {} configures several backends but names no default",
        path.display()
    )]
    MissingDefault {
        /// The file.
        path: PathBuf,
    },
    /// A configuration built in code names no default among its backends. A file never gets
    /// here: [`Config::load`] refuses it with [`NoBackends`](Self::NoBackends),
    /// [`UnknownDefault`](Self::UnknownDefault) or [`MissingDefault`](Self::MissingDefault).
    #[error("the configuration names no default backend it configures")]
    NoDefault,
}

impl Config {
    /// Reads the configuration at `path`; a file that does not exist yields
    /// [`Config::builtin`].
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Io`] when the file exists but cannot be read,
    /// [`ConfigError::Parse`] when it is not valid TOML, has a key Partitur does not know, a
    /// value of the wrong kind, an invalid backend name or a backend without a `command`,
    /// [`ConfigError::NoBackends`] when it configures no backend,
    /// [`ConfigError::UnknownDefault`] when its `default` names no configured backend and
    /// [`ConfigError::MissingDefault`] when it configures several backends without a
    /// `default`.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let dir = tempfile::TempDir::new()?;
    /// let path = dir.path().join("config.toml");
    /// std::fs::write(
    ///     &path,
    ///     "default = \"main\"\n\
    ///      [backends.main]\ncommand = \"claude-agent-acp\"\n\
    ///      [backends.work]\ncommand = \"claude-agent-acp\"\n\
    ///      env = { CLAUDE_CONFIG_DIR = \"~/.claude-work\" }\n",
    /// )?;
    ///
    /// let config = Config::load(&path)?;
    /// assert_eq!(config.backends.len(), 2);
    /// assert_eq!(config.default.map(|id| id.to_string()).as_deref(), Some("main"));
    /// ```
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::builtin()),
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let config: Self = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        if config.effective_default().is_some() {
            return Ok(config);
        }
        let path = path.to_path_buf();
        Err(match config.default {
            _ if config.backends.is_empty() => ConfigError::NoBackends { path },
            Some(default) => ConfigError::UnknownDefault { path, default },
            None => ConfigError::MissingDefault { path },
        })
    }

    /// The configuration without a file: one backend named `claude` running the Claude Code
    /// adapter `claude-agent-acp`, without arguments or extra environment and with summarized
    /// thinking.
    pub fn builtin() -> Self {
        // The name follows the id rule, so this is always `Some`.
        let claude = BackendId::try_from(BUILTIN_BACKEND).ok();
        let adapter = BackendConfig {
            command: PathBuf::from(ADAPTER_COMMAND),
            args: Vec::new(),
            env: BTreeMap::new(),
            thinking: ThinkingDisplay::Summarized,
        };
        Self {
            backends: claude
                .iter()
                .map(|id| (id.clone(), adapter.clone()))
                .collect(),
            default: claude,
            worktree_path: None,
            link_opener: None,
        }
    }

    /// The backend new sessions run on: `default` when it names a configured backend, else
    /// the only configured one; `None` when neither applies.
    fn effective_default(&self) -> Option<BackendId> {
        match &self.default {
            Some(default) => self.backends.contains_key(default).then(|| default.clone()),
            None if self.backends.len() == 1 => self.backends.keys().next().cloned(),
            None => None,
        }
    }
}

/// The global settings layer `config` sets: its `worktree_path` and `link_opener`, each when
/// it has one. `path` is the config file, named in the error.
///
/// # Errors
///
/// Returns [`ConfigError::Setting`] when `worktree_path` is not a valid pattern or
/// `link_opener` names no program.
pub fn global_settings(config: &Config, path: &Path) -> Result<SettingsLayer, ConfigError> {
    let mut layer = SettingsLayer::default();
    let keys = [
        ("worktree_path", &config.worktree_path),
        ("link_opener", &config.link_opener),
    ];
    for (key, value) in keys {
        if let Some(value) = value {
            layer
                .set(Level::Global, key, value)
                .map_err(|source| ConfigError::Setting {
                    path: path.to_path_buf(),
                    key,
                    source,
                })?;
        }
    }
    Ok(layer)
}

/// The backends `config` names, as the core and the UI show them sorted by id, and the one new
/// sessions run on.
///
/// The list shows each backend's command as configured, even when it runs through `npx`.
///
/// # Errors
///
/// Returns [`ConfigError::NoDefault`] when `config` names no default it configures (which a
/// loaded configuration always does).
pub fn listing(config: &Config) -> Result<(Vec<BackendInfo>, BackendId), ConfigError> {
    let default = config.effective_default().ok_or(ConfigError::NoDefault)?;
    let infos = config
        .backends
        .iter()
        .map(|(id, entry)| BackendInfo {
            id: id.clone(),
            name: id.to_string(),
            command: entry.command.display().to_string(),
        })
        .collect();
    Ok((infos, default))
}

/// Where a bare adapter command is looked up and how a found command runs.
///
/// Off Windows `extensions` is empty and `full_path` false: a bare name is a file of a folder
/// and runs as configured. On Windows a name without an extension is tried only as `name + ext`
/// for each of `extensions` in order, never as written, so `npx` finds `npx.cmd` and not Node's
/// sh script `npx` beside it, and a found command runs by its full path. An extension is tried
/// as listed and then in lowercase, since the file system there ignores case and the list is
/// uppercase by default (`.CMD` finds `npx.cmd`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lookup {
    /// The folders searched, in order.
    pub folders: Vec<PathBuf>,
    /// The extensions a name without one is tried with, in order; empty off Windows.
    pub extensions: Vec<String>,
    /// Whether a found command runs by the found file's path instead of as configured.
    pub full_path: bool,
}

/// The extensions tried when no `PATHEXT` is set.
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// Whether the environment variable names `a` and `b` are the same, ignoring case when
/// `fold_case` (Windows names are case-insensitive, others are not).
pub fn env_name_eq(a: &str, b: &str, fold_case: bool) -> bool {
    if fold_case {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

impl Lookup {
    /// The lookup for the environment `base`: the folders of its last `PATH` (the one a child
    /// process gets), else those of `process_path`; on Windows (`windows`) the names are
    /// compared ignoring case, the extensions come from the last `PATHEXT` of `base`, else
    /// `process_pathext`, else `.COM;.EXE;.BAT;.CMD`, split on `;` without empty entries, and
    /// found commands run by their full path.
    pub fn from_env(
        base: &[(String, String)],
        process_path: Option<OsString>,
        process_pathext: Option<OsString>,
        windows: bool,
    ) -> Self {
        let last = |wanted: &str| {
            base.iter()
                .rev()
                .find(|(name, _)| env_name_eq(name, wanted, windows))
                .map(|(_, value)| OsString::from(value))
        };
        let folders = last("PATH")
            .or(process_path)
            .map(|path| env::split_paths(&path).collect())
            .unwrap_or_default();
        let split = |list: &str| -> Vec<String> {
            list.split(';')
                .filter(|ext| !ext.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let extensions = if windows {
            let listed = last("PATHEXT")
                .or(process_pathext)
                .map(|ext| split(&ext.to_string_lossy()))
                .unwrap_or_default();
            // A list with nothing in it counts as none, so a bare name is never tried as written.
            if listed.is_empty() {
                split(DEFAULT_PATHEXT)
            } else {
                listed
            }
        } else {
            Vec::new()
        };
        Self {
            folders,
            extensions,
            full_path: windows,
        }
    }

    /// The file `command` names in one of the folders, the first folder's first match.
    pub(crate) fn find(&self, command: &Path) -> Option<PathBuf> {
        self.folders
            .iter()
            .find_map(|folder| self.find_in(folder, command))
    }

    /// The file `command` names in `folder`, if any.
    fn find_in(&self, folder: &Path, command: &Path) -> Option<PathBuf> {
        if self.extensions.is_empty() || command.extension().is_some() {
            return Some(folder.join(command)).filter(|file| file.is_file());
        }
        self.extensions
            .iter()
            .flat_map(|ext| [ext.clone(), ext.to_lowercase()])
            .map(|ext| {
                let mut name = command.as_os_str().to_owned();
                name.push(ext);
                folder.join(name)
            })
            .find(|file| file.is_file())
    }
}

/// The user's home folder; without one a `~` stays as written.
fn home_folder() -> PathBuf {
    BaseDirs::new().map_or_else(|| PathBuf::from("~"), |dirs| dirs.home_dir().into())
}

/// The lookup for `base` with this process's `PATH` and `PATHEXT` as the fallback.
fn process_lookup(base: &[(String, String)]) -> Lookup {
    Lookup::from_env(
        base,
        env::var_os("PATH"),
        env::var_os("PATHEXT"),
        cfg!(windows),
    )
}

/// Resolves every backend `config` names on top of the process environment `base`, looking
/// commands up on its `PATH` (else this process's) and expanding `~` to the user's home folder.
///
/// See [`resolve_backends_with`].
///
/// # Errors
///
/// Returns the errors [`resolve_backends_with`] does.
pub fn resolve_backends(
    config: &Config,
    base: &[(String, String)],
) -> Result<Resolved, ConfigError> {
    resolve_backends_with(config, &process_lookup(base), &home_folder(), base)
}

/// The backends resolved at the start, or `None` when `shell_asked`: then the login shell is
/// being asked for the environment and the backends are resolved with [`resolution`] once it
/// has answered, so a missing command fails only the sessions of its backend instead of the
/// start. That holds even when `env` is known already, as after a lookup that failed at once.
/// Without the shell, `env` is known from the start and this does not wait.
///
/// # Errors
///
/// Returns the errors [`resolve_backends`] does when the shell is not asked.
pub fn resolve_at_start(
    config: &Config,
    shell_asked: bool,
    env: &ProcessEnv,
) -> Result<Option<Resolved>, ConfigError> {
    if shell_asked {
        return Ok(None);
    }
    resolve_backends(config, &env.wait()).map(Some)
}

/// What the adapter runtime starts sessions with once the process environment `base` is known:
/// every backend `config` names, resolved as [`resolve_backends`] does, a backend whose command
/// is missing carrying that error's message instead. Opening a session on such a backend then
/// fails with the message while every other backend works.
pub fn resolution(config: &Config, base: &[(String, String)]) -> Resolution {
    resolve_each(config, &process_lookup(base), &home_folder(), base)
        .into_iter()
        .map(|(id, result)| (id, result.map_err(|error| error.to_string())))
        .collect()
}

/// Resolves each backend `config` names on its own, as [`resolve_backends_with`] does, so a
/// backend whose command is missing has its [`ConfigError::CommandNotFound`] and leaves the
/// others resolved.
pub fn resolve_each(
    config: &Config,
    lookup: &Lookup,
    home: &Path,
    base: &[(String, String)],
) -> HashMap<BackendId, Result<AcpConfig, ConfigError>> {
    resolve_in_order(config, lookup, home, base).collect()
}

/// Every backend `config` names with its resolution, in id order.
fn resolve_in_order<'a>(
    config: &'a Config,
    lookup: &'a Lookup,
    home: &'a Path,
    base: &'a [(String, String)],
) -> impl Iterator<Item = (BackendId, Result<AcpConfig, ConfigError>)> + 'a {
    config
        .backends
        .iter()
        .map(move |(id, entry)| (id.clone(), resolve_entry(id, entry, lookup, home, base)))
}

/// Resolves every backend `config` names, looking commands up with
/// `lookup` and expanding `~` to `home`.
///
/// A command with a folder in it runs as configured, as does a bare name found as a file in
/// one of the folders. The default adapter command found nowhere falls back to running the
/// pinned Claude Code adapter package through `npx`, with the configured arguments after the
/// package name. Each backend's environment is `base` followed by its `env` in name order, so
/// a configured variable wins over one of `base`, a value of `~` or starting with `~/` having
/// that `~` replaced by `home`; the adapter's transport sets the thinking variable after them.
/// Each resolved backend is logged with its command and whether the fallback applied.
///
/// The backend list is [`listing`]'s.
///
/// # Errors
///
/// Returns [`ConfigError::CommandNotFound`] for the first backend, in id order, whose command
/// is a bare name other than the default adapter command found nowhere, and
/// [`ConfigError::NoDefault`] when `config` names no default it configures (which a loaded
/// configuration always does).
pub fn resolve_backends_with(
    config: &Config,
    lookup: &Lookup,
    home: &Path,
    base: &[(String, String)],
) -> Result<Resolved, ConfigError> {
    let (infos, default) = listing(config)?;
    let backends = resolve_in_order(config, lookup, home, base)
        .map(|(id, result)| result.map(|acp| (id, acp)))
        .collect::<Result<_, _>>()?;
    Ok(Resolved {
        backends,
        infos,
        default,
    })
}

/// How to start the adapter of the backend `id`, configured as `entry`.
fn resolve_entry(
    id: &BackendId,
    entry: &BackendConfig,
    lookup: &Lookup,
    home: &Path,
    base: &[(String, String)],
) -> Result<AcpConfig, ConfigError> {
    let has_folder = entry
        .command
        .parent()
        .is_some_and(|folder| !folder.as_os_str().is_empty());
    let found = if has_folder {
        None
    } else {
        lookup.find(&entry.command)
    };
    let installed = has_folder || found.is_some();
    let npx = !installed && entry.command == Path::new(ADAPTER_COMMAND);
    let (command, args) = if installed {
        let command = found
            .filter(|_| lookup.full_path)
            .unwrap_or_else(|| entry.command.clone());
        (command, entry.args.clone())
    } else if npx {
        let args = ["--yes", ADAPTER_PACKAGE]
            .into_iter()
            .map(str::to_owned)
            .chain(entry.args.iter().cloned())
            .collect();
        let command = lookup
            .find(Path::new("npx"))
            .filter(|_| lookup.full_path)
            .unwrap_or_else(|| PathBuf::from("npx"));
        (command, args)
    } else {
        return Err(ConfigError::CommandNotFound {
            backend: id.clone(),
            command: entry.command.clone(),
        });
    };
    tracing::info!(backend = %id, command = %entry.command.display(), npx, "resolved a backend");
    let env = base
        .iter()
        .cloned()
        .chain(
            entry
                .env
                .iter()
                .map(|(name, value)| (name.clone(), expand_home(value, home))),
        )
        .collect();
    Ok(AcpConfig {
        command,
        args,
        env,
        thinking_display: entry.thinking,
    })
}

/// `value` with a leading `~` (alone or followed by `/` or `\`) replaced by `home`; any other
/// value unchanged. Extra separators after the `~` are dropped, so the result stays under `home`
/// (joining an absolute rest would replace `home` altogether).
fn expand_home(value: &str, home: &Path) -> String {
    if value == "~" {
        home.display().to_string()
    } else if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        home.join(rest.trim_start_matches(['/', '\\']))
            .display()
            .to_string()
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::fs;
    use std::sync::Arc;

    use partitur_core::{LinkOpenerError, PatternError};
    use tempfile::TempDir;

    use super::*;

    const TWO_BACKENDS: &str = r#"
default = "claude-main"

[backends.claude-main]
command = "claude-agent-acp"
env = { CLAUDE_CONFIG_DIR = "~/.claude-main" }

[backends.claude-work]
command = "/opt/acp/bin/agent"
args = ["--verbose"]
env = { CLAUDE_CONFIG_DIR = "~/.claude-work", AGENT_LOG = "1" }
thinking = "omitted"
"#;

    fn config_file(content: &str) -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, content).unwrap();
        (dir, path)
    }

    fn id(name: &str) -> BackendId {
        BackendId::try_from(name).unwrap()
    }

    fn entry(command: &str, args: &[&str]) -> BackendConfig {
        BackendConfig {
            command: PathBuf::from(command),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            env: BTreeMap::new(),
            thinking: ThinkingDisplay::Omitted,
        }
    }

    /// A configuration of the given entries without a `default`.
    fn config_of(entries: &[(&str, BackendConfig)]) -> Config {
        Config {
            default: None,
            worktree_path: None,
            link_opener: None,
            backends: entries
                .iter()
                .map(|(name, entry)| (id(name), entry.clone()))
                .collect(),
        }
    }

    fn home() -> PathBuf {
        PathBuf::from("/home/me")
    }

    #[test]
    fn a_top_level_worktree_path_becomes_the_global_layer() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            "worktree_path = \"~/wt/{branch}\"\n[backends.a]\ncommand=\"x\"",
        )
        .unwrap();
        let config = Config::load(&path).unwrap();

        let layer = global_settings(&config, &path).unwrap();

        assert_eq!(layer.worktree_path().unwrap().as_str(), "~/wt/{branch}");
    }

    #[test]
    fn an_unknown_placeholder_in_config_stops_the_start() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            "worktree_path = \"~/wt/{nope}\"\n[backends.a]\ncommand=\"x\"",
        )
        .unwrap();
        let config = Config::load(&path).unwrap();

        let error = global_settings(&config, &path).unwrap_err();

        assert!(matches!(
            &error,
            ConfigError::Setting { path: p, key: "worktree_path", source: SettingError::Pattern(PatternError::UnknownPlaceholder(name)) }
                if *p == path && name == "nope"
        ));
        assert!(
            error
                .to_string()
                .starts_with("invalid worktree_path in the config file")
        );
    }

    #[test]
    fn without_the_key_the_global_layer_is_empty() {
        let layer = global_settings(&Config::builtin(), Path::new("config.toml")).unwrap();

        assert_eq!(layer.worktree_path(), None);
        assert_eq!(layer.link_opener(), None);
    }

    #[test]
    fn a_top_level_link_opener_becomes_the_global_layer() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            "link_opener = \"firefox -P work\"\n[backends.a]\ncommand=\"x\"",
        )
        .unwrap();
        let config = Config::load(&path).unwrap();

        let layer = global_settings(&config, &path).unwrap();

        assert_eq!(layer.link_opener().unwrap().to_string(), "firefox -P work");
        assert_eq!(layer.worktree_path(), None);
    }

    #[test]
    fn a_link_opener_without_a_program_stops_the_start() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            "worktree_path = \"~/wt/{branch}\"\nlink_opener = \" \"\n[backends.a]\ncommand=\"x\"",
        )
        .unwrap();
        let config = Config::load(&path).unwrap();

        let error = global_settings(&config, &path).unwrap_err();

        assert!(matches!(
            &error,
            ConfigError::Setting { path: p, key: "link_opener", source: SettingError::LinkOpener(LinkOpenerError::Empty) }
                if *p == path
        ));
        assert!(
            error
                .to_string()
                .starts_with("invalid link_opener in the config file")
        );
    }

    #[test]
    fn a_missing_config_is_the_builtin_claude_backend() {
        let dir = TempDir::new().unwrap();

        let config = Config::load(&dir.path().join("config.toml")).unwrap();

        assert_eq!(config, Config::builtin());
        assert_eq!(config.default, Some(id("claude")));
        assert_eq!(
            config.backends,
            BTreeMap::from([(
                id("claude"),
                BackendConfig {
                    command: PathBuf::from("claude-agent-acp"),
                    args: Vec::new(),
                    env: BTreeMap::new(),
                    thinking: ThinkingDisplay::Summarized,
                }
            )])
        );
    }

    #[test]
    fn the_backends_table_parses_with_optional_fields() {
        let (_dir, path) = config_file(TWO_BACKENDS);

        let config = Config::load(&path).unwrap();

        assert_eq!(config.default, Some(id("claude-main")));
        assert_eq!(
            config.backends,
            BTreeMap::from([
                (
                    id("claude-main"),
                    BackendConfig {
                        command: PathBuf::from("claude-agent-acp"),
                        args: Vec::new(),
                        env: BTreeMap::from([(
                            "CLAUDE_CONFIG_DIR".to_owned(),
                            "~/.claude-main".to_owned()
                        )]),
                        thinking: ThinkingDisplay::Summarized,
                    }
                ),
                (
                    id("claude-work"),
                    BackendConfig {
                        command: PathBuf::from("/opt/acp/bin/agent"),
                        args: vec!["--verbose".to_owned()],
                        env: BTreeMap::from([
                            ("AGENT_LOG".to_owned(), "1".to_owned()),
                            ("CLAUDE_CONFIG_DIR".to_owned(), "~/.claude-work".to_owned()),
                        ]),
                        thinking: ThinkingDisplay::Omitted,
                    }
                ),
            ])
        );
    }

    #[test]
    fn one_backend_needs_no_default_two_do() {
        let (_dir, path) = config_file("[backends.solo]\ncommand = \"/opt/agent\"\n");
        let config = Config::load(&path).unwrap();
        assert_eq!(config.default, None);
        let resolved = resolve_backends_with(&config, &Lookup::default(), &home(), &[]).unwrap();
        assert_eq!(resolved.default, id("solo"));

        let (_dir, path) =
            config_file("[backends.a]\ncommand = \"/opt/a\"\n[backends.b]\ncommand = \"/opt/b\"\n");
        let error = Config::load(&path).unwrap_err();
        assert!(matches!(&error, ConfigError::MissingDefault { path: named } if *named == path));
        assert_eq!(
            error.to_string(),
            format!(
                "the config file {} configures several backends but names no default",
                path.display()
            )
        );

        let (_dir, path) = config_file(
            "default = \"b\"\n[backends.a]\ncommand = \"/opt/a\"\n[backends.b]\ncommand = \"/opt/b\"\n",
        );
        let resolved = resolve_backends_with(
            &Config::load(&path).unwrap(),
            &Lookup::default(),
            &home(),
            &[],
        )
        .unwrap();
        assert_eq!(resolved.default, id("b"));
    }

    #[test]
    fn an_unknown_default_names_the_file() {
        for content in [
            "default = \"gone\"\n[backends.solo]\ncommand = \"/opt/agent\"\n",
            "default = \"gone\"\n[backends.a]\ncommand = \"/opt/a\"\n[backends.b]\ncommand = \"/opt/b\"\n",
        ] {
            let (_dir, path) = config_file(content);

            let error = Config::load(&path).unwrap_err();

            assert!(
                matches!(&error, ConfigError::UnknownDefault { path: named, default } if *named == path && *default == id("gone")),
                "{content}"
            );
            assert_eq!(
                error.to_string(),
                format!(
                    "the config file {} names gone as the default backend, which it does not configure",
                    path.display()
                )
            );
        }
    }

    #[test]
    fn an_empty_table_is_an_error() {
        for content in ["", "[backends]\n", "default = \"claude\"\n[backends]\n"] {
            let (_dir, path) = config_file(content);

            let error = Config::load(&path).unwrap_err();

            assert!(
                matches!(&error, ConfigError::NoBackends { path: named } if *named == path),
                "{content:?}"
            );
            assert_eq!(
                error.to_string(),
                format!("the config file {} configures no backend", path.display())
            );
        }
    }

    #[test]
    fn the_old_single_backend_block_is_rejected() {
        let (_dir, path) = config_file("[backend]\ncommand = \"claude-agent-acp\"\n");

        let error = Config::load(&path).unwrap_err();

        assert!(matches!(&error, ConfigError::Parse { path: named, .. } if *named == path));
    }

    #[test]
    fn an_invalid_name_a_missing_command_or_an_unknown_key_is_a_parse_error() {
        for content in [
            "[backends.\"a/b\"]\ncommand = \"/opt/agent\"\n",
            "[backends.\"\"]\ncommand = \"/opt/agent\"\n",
            "default = \"a/b\"\n[backends.a]\ncommand = \"/opt/agent\"\n",
            "[backends.a]\nargs = []\n",
            "[backends.a]\ncommand = \"/opt/agent\"\ncomand = \"x\"\n",
            "[backends.a]\ncommand = \"/opt/agent\"\nthinking = \"verbose\"\n",
            "[backends.a]\ncommand = \"/opt/agent\"\nenv = { A = 1 }\n",
            "colour = \"blue\"\n[backends.a]\ncommand = \"/opt/agent\"\n",
        ] {
            let (_dir, path) = config_file(content);

            let error = Config::load(&path).unwrap_err();

            assert!(
                matches!(&error, ConfigError::Parse { path: named, .. } if *named == path),
                "{content}"
            );
            assert!(error.to_string().contains(&path.display().to_string()));
        }
    }

    #[test]
    fn malformed_toml_is_a_parse_error_with_the_path() {
        let (_dir, path) = config_file("[backends\ncommand = ");

        let error = Config::load(&path).unwrap_err();

        assert!(matches!(&error, ConfigError::Parse { path: named, .. } if *named == path));
        assert!(error.to_string().contains(&path.display().to_string()));
    }

    #[test]
    fn an_unreadable_config_is_an_io_error_with_the_path() {
        let dir = TempDir::new().unwrap();

        let error = Config::load(dir.path()).unwrap_err();

        assert!(matches!(&error, ConfigError::Io { path, .. } if path == dir.path()));
        assert!(
            error
                .to_string()
                .contains(&dir.path().display().to_string())
        );
    }

    #[test]
    fn tilde_expands_only_at_the_start_of_a_value() {
        let mut work = entry("/opt/agent", &[]);
        work.env = BTreeMap::from([
            ("A_DIR".to_owned(), "~/x".to_owned()),
            ("B_HOME".to_owned(), "~".to_owned()),
            ("C_PATH".to_owned(), "/opt/~x".to_owned()),
            ("D_USER".to_owned(), "~user".to_owned()),
            ("E_NESTED".to_owned(), "~/a/~/b".to_owned()),
            ("F_SLASHES".to_owned(), "~//x".to_owned()),
            ("G_BACKSLASH".to_owned(), r"~\bin".to_owned()),
            ("H_INNER".to_owned(), r"a~\b".to_owned()),
        ]);

        let resolved = resolve_backends_with(
            &config_of(&[("work", work)]),
            &Lookup::default(),
            &home(),
            &[],
        )
        .unwrap();

        let expected: Vec<(String, String)> = [
            ("A_DIR", "/home/me/x"),
            ("B_HOME", "/home/me"),
            ("C_PATH", "/opt/~x"),
            ("D_USER", "~user"),
            ("E_NESTED", "/home/me/a/~/b"),
            ("F_SLASHES", "/home/me/x"),
            ("G_BACKSLASH", "/home/me/bin"),
            ("H_INNER", r"a~\b"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
        assert_eq!(resolved.backends[&id("work")].env, expected);
    }

    #[test]
    fn thinking_is_applied_after_env() {
        let mut work = entry("/opt/agent", &[]);
        work.env = BTreeMap::from([
            (
                "CLAUDE_CODE_THINKING_DISPLAY".to_owned(),
                "summarized".to_owned(),
            ),
            ("AGENT_LOG".to_owned(), "1".to_owned()),
        ]);

        let resolved = resolve_backends_with(
            &config_of(&[("work", work)]),
            &Lookup::default(),
            &home(),
            &[],
        )
        .unwrap();

        // The configured variables go first, in name order; the adapter's transport sets
        // `CLAUDE_CODE_THINKING_DISPLAY` from `thinking_display` after them, so `thinking`
        // wins over a variable of the same name.
        let acp = &resolved.backends[&id("work")];
        assert_eq!(
            acp.env,
            [
                ("AGENT_LOG".to_owned(), "1".to_owned()),
                (
                    "CLAUDE_CODE_THINKING_DISPLAY".to_owned(),
                    "summarized".to_owned()
                ),
            ]
        );
        assert_eq!(acp.thinking_display, ThinkingDisplay::Omitted);
    }

    #[test]
    fn the_npx_fallback_applies_per_default_command_entry() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("claude-agent-acp")).unwrap();
        fs::create_dir(dir.path().join("my-adapter")).unwrap();
        let path = [dir.path().to_path_buf()];
        let fallback = entry("claude-agent-acp", &["-v"]);

        let resolved = resolve_backends_with(
            &config_of(&[("main", fallback.clone())]),
            &plain(&path),
            &home(),
            &[],
        )
        .unwrap();
        let acp = &resolved.backends[&id("main")];
        assert_eq!(acp.command, PathBuf::from("npx"));
        assert_eq!(
            acp.args,
            [
                "--yes",
                "@agentclientprotocol/claude-agent-acp@0.101.0",
                "-v"
            ]
        );
        assert_eq!(acp.thinking_display, ThinkingDisplay::Omitted);
        // The list shows what the user configured, not the fallback.
        assert_eq!(resolved.infos[0].command, "claude-agent-acp");

        let mut both = config_of(&[("main", fallback), ("custom", entry("my-adapter", &[]))]);
        both.default = Some(id("main"));
        let error = resolve_backends_with(&both, &plain(&path), &home(), &[]).unwrap_err();
        assert!(matches!(
            &error,
            ConfigError::CommandNotFound { backend, command }
                if *backend == id("custom") && *command == Path::new("my-adapter")
        ));
        assert_eq!(
            error.to_string(),
            "the adapter command my-adapter of backend custom is not on PATH"
        );
    }

    #[test]
    fn a_command_found_on_the_path_runs_as_configured() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("claude-agent-acp"), "").unwrap();
        fs::write(dir.path().join("my-adapter"), "").unwrap();
        let path = [PathBuf::from("/nowhere"), dir.path().to_path_buf()];
        let mut config = config_of(&[
            ("main", entry("claude-agent-acp", &["-v"])),
            ("custom", entry("my-adapter", &[])),
        ]);
        config.default = Some(id("custom"));

        let resolved = resolve_backends_with(&config, &plain(&path), &home(), &[]).unwrap();

        let main = &resolved.backends[&id("main")];
        assert_eq!(main.command, PathBuf::from("claude-agent-acp"));
        assert_eq!(main.args, ["-v"]);
        assert!(main.env.is_empty());
        assert_eq!(
            resolved.backends[&id("custom")].command,
            PathBuf::from("my-adapter")
        );
        assert_eq!(resolved.default, id("custom"));
    }

    #[test]
    fn a_command_with_a_folder_runs_as_configured_even_when_missing() {
        for command in ["/opt/missing/agent", "bin/agent"] {
            let resolved = resolve_backends_with(
                &config_of(&[("a", entry(command, &[]))]),
                &Lookup::default(),
                &home(),
                &[],
            )
            .unwrap();

            let acp = &resolved.backends[&id("a")];
            assert_eq!(acp.command, PathBuf::from(command));
            assert!(acp.args.is_empty());
        }
    }

    #[test]
    fn the_list_is_sorted_by_id_and_shows_the_configured_command() {
        let (_dir, path) = config_file(TWO_BACKENDS);
        let config = Config::load(&path).unwrap();

        let resolved = resolve_backends_with(&config, &Lookup::default(), &home(), &[]).unwrap();

        assert_eq!(
            resolved.infos,
            [
                BackendInfo {
                    id: id("claude-main"),
                    name: "claude-main".into(),
                    command: "claude-agent-acp".into(),
                },
                BackendInfo {
                    id: id("claude-work"),
                    name: "claude-work".into(),
                    command: "/opt/acp/bin/agent".into(),
                },
            ]
        );
        let mut keys: Vec<_> = resolved.backends.keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, [id("claude-main"), id("claude-work")]);
        assert_eq!(resolved.default, id("claude-main"));
        let work = &resolved.backends[&id("claude-work")];
        assert_eq!(work.args, ["--verbose"]);
        assert_eq!(
            work.env,
            [
                ("AGENT_LOG".to_owned(), "1".to_owned()),
                (
                    "CLAUDE_CONFIG_DIR".to_owned(),
                    "/home/me/.claude-work".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn a_configuration_built_in_code_without_a_usable_default_is_refused() {
        let mut unknown = config_of(&[("a", entry("/opt/a", &[]))]);
        unknown.default = Some(id("gone"));
        let several = config_of(&[("a", entry("/opt/a", &[])), ("b", entry("/opt/b", &[]))]);

        for config in [config_of(&[]), unknown, several] {
            let error =
                resolve_backends_with(&config, &Lookup::default(), &home(), &[]).unwrap_err();

            assert!(matches!(error, ConfigError::NoDefault), "{config:?}");
            assert_eq!(
                error.to_string(),
                "the configuration names no default backend it configures"
            );
        }
    }

    #[test]
    fn each_resolved_backend_is_logged_at_info() {
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("log");
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .with_writer(Arc::new(fs::File::create(&log).unwrap()))
            .finish();
        let mut config = config_of(&[
            ("main", entry("claude-agent-acp", &[])),
            ("work", entry("/opt/agent", &[])),
        ]);
        config.default = Some(id("main"));

        // While only one subscriber exists, tracing decides whether a log call is enabled
        // from the subscriber of whichever thread hits the call first. Other tests resolve
        // backends on other threads without one, and could cache it as disabled for good. A
        // second, silent subscriber makes tracing ask every live subscriber instead.
        let _silent = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        tracing::subscriber::with_default(subscriber, || {
            resolve_backends_with(&config, &Lookup::default(), &home(), &[]).unwrap();
        });

        let written = fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), 2, "{written}");
        assert!(lines[0].contains("INFO"), "{written}");
        assert!(lines[0].contains("backend=main"), "{written}");
        assert!(lines[0].contains("command=claude-agent-acp"), "{written}");
        assert!(lines[0].contains("npx=true"), "{written}");
        assert!(lines[1].contains("backend=work"), "{written}");
        assert!(lines[1].contains("command=/opt/agent"), "{written}");
        assert!(lines[1].contains("npx=false"), "{written}");
    }

    fn pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn the_base_env_comes_before_the_configured_env() {
        let mut work = entry("/opt/agent", &[]);
        work.env = BTreeMap::from([
            ("A_DIR".to_owned(), "~/a".to_owned()),
            ("PATH".to_owned(), "/configured".to_owned()),
        ]);
        let base = pairs(&[("PATH", "/shell/bin"), ("Z_TOKEN", "t")]);

        let resolved = resolve_backends_with(
            &config_of(&[("work", work)]),
            &Lookup::default(),
            &home(),
            &base,
        )
        .unwrap();

        // Later entries win, so a configured variable overrides the shell's of the same name.
        assert_eq!(
            resolved.backends[&id("work")].env,
            pairs(&[
                ("PATH", "/shell/bin"),
                ("Z_TOKEN", "t"),
                ("A_DIR", "/home/me/a"),
                ("PATH", "/configured"),
            ])
        );
    }

    fn plain(folders: &[PathBuf]) -> Lookup {
        Lookup {
            folders: folders.to_vec(),
            ..Lookup::default()
        }
    }

    fn windows(dir: &TempDir) -> Lookup {
        let base = pairs(&[("Path", &dir.path().display().to_string())]);
        Lookup::from_env(&base, None, None, true)
    }

    fn touch(dir: &TempDir, names: &[&str]) {
        for name in names {
            fs::write(dir.path().join(name), "").unwrap();
        }
    }

    #[test]
    fn the_lookup_prefers_the_base_path() {
        let base = pairs(&[("HOME", "/h"), ("PATH", "/a:/b"), ("PATH", "/c:/d")]);

        let lookup = Lookup::from_env(&base, Some("/process".into()), None, false);

        // The last `PATH` is the one a child gets.
        assert_eq!(lookup.folders, [PathBuf::from("/c"), PathBuf::from("/d")]);
        assert!(lookup.extensions.is_empty());
        assert!(!lookup.full_path);
    }

    #[test]
    fn the_lookup_falls_back_to_the_process_path() {
        let base = pairs(&[("HOME", "/h")]);

        assert_eq!(
            Lookup::from_env(&base, Some("/p:/q".into()), None, false).folders,
            [PathBuf::from("/p"), PathBuf::from("/q")]
        );
        assert!(
            Lookup::from_env(&base, None, None, false)
                .folders
                .is_empty()
        );
    }

    #[test]
    fn env_name_eq_folds_case_only_when_asked() {
        assert!(env_name_eq("Path", "PATH", true));
        assert!(!env_name_eq("Path", "PATH", false));
        assert!(env_name_eq("PATH", "PATH", false));
        assert!(!env_name_eq("PATHS", "PATH", true));
    }

    #[test]
    fn the_windows_lookup_reads_path_in_any_case() {
        let base = pairs(&[("Path", "/win")]);

        let lookup = Lookup::from_env(&base, Some("/process".into()), None, true);

        assert_eq!(lookup.folders, [PathBuf::from("/win")]);
        assert!(lookup.full_path);
    }

    #[test]
    fn another_system_ignores_a_path_of_another_case() {
        let base = pairs(&[("Path", "/win")]);

        let lookup = Lookup::from_env(&base, Some("/process".into()), None, false);

        assert_eq!(lookup.folders, [PathBuf::from("/process")]);
    }

    #[test]
    fn pathext_defaults_and_splits() {
        let default = [".COM", ".EXE", ".BAT", ".CMD"];
        assert_eq!(Lookup::from_env(&[], None, None, true).extensions, default);
        assert_eq!(
            Lookup::from_env(&[], None, Some(".EXE;;.CMD".into()), true).extensions,
            [".EXE", ".CMD"]
        );
        for empty in ["", ";", ";;"] {
            assert_eq!(
                Lookup::from_env(&pairs(&[("PATHEXT", empty)]), None, None, true).extensions,
                default
            );
            assert_eq!(
                Lookup::from_env(&[], None, Some(empty.into()), true).extensions,
                default
            );
        }
        let base = pairs(&[("PathExt", ".A"), ("PATHEXT", ".B;.C")]);
        assert_eq!(
            Lookup::from_env(&base, None, Some(".EXE".into()), true).extensions,
            [".B", ".C"]
        );
        assert!(
            Lookup::from_env(&base, None, Some(".EXE".into()), false)
                .extensions
                .is_empty()
        );
    }

    #[test]
    fn the_windows_lookup_prefers_the_cmd_shim_over_the_sh_script() {
        let dir = TempDir::new().unwrap();
        touch(&dir, &["npx", "npx.cmd", "npx.ps1"]);
        let config = config_of(&[("claude", entry("claude-agent-acp", &["-v"]))]);

        let resolved = resolve_backends_with(&config, &windows(&dir), &home(), &[]).unwrap();

        let acp = &resolved.backends[&id("claude")];
        assert_eq!(acp.command, dir.path().join("npx.cmd"));
        assert_eq!(
            acp.args,
            [
                "--yes",
                "@agentclientprotocol/claude-agent-acp@0.101.0",
                "-v"
            ]
        );
    }

    #[test]
    fn without_npx_the_windows_fallback_stays_the_bare_name() {
        let dir = TempDir::new().unwrap();
        touch(&dir, &["npx"]);
        let config = config_of(&[("claude", entry("claude-agent-acp", &[]))]);

        let resolved = resolve_backends_with(&config, &windows(&dir), &home(), &[]).unwrap();

        assert_eq!(
            resolved.backends[&id("claude")].command,
            PathBuf::from("npx")
        );
    }

    #[test]
    fn a_bare_command_found_through_pathext_runs_by_its_full_path() {
        let dir = TempDir::new().unwrap();
        touch(&dir, &["claude-agent-acp", "claude-agent-acp.cmd"]);
        let config = config_of(&[("claude", entry("claude-agent-acp", &[]))]);

        let resolved = resolve_backends_with(&config, &windows(&dir), &home(), &[]).unwrap();

        let acp = &resolved.backends[&id("claude")];
        assert_eq!(acp.command, dir.path().join("claude-agent-acp.cmd"));
        assert!(acp.args.is_empty());
    }

    #[test]
    fn the_extensions_are_tried_in_order_and_in_lowercase() {
        let dir = TempDir::new().unwrap();
        touch(&dir, &["tool.bat", "tool.exe"]);
        let lookup = Lookup {
            folders: vec![PathBuf::from("/nowhere"), dir.path().to_path_buf()],
            extensions: vec![".BAT".to_owned(), ".EXE".to_owned()],
            full_path: true,
        };

        assert_eq!(
            lookup.find(Path::new("tool")),
            Some(dir.path().join("tool.bat"))
        );
        assert_eq!(lookup.find(Path::new("other")), None);
    }

    #[test]
    fn a_command_with_an_extension_is_tried_as_written() {
        let dir = TempDir::new().unwrap();
        touch(&dir, &["tool.exe"]);
        let config = config_of(&[("a", entry("tool.exe", &[]))]);

        let resolved = resolve_backends_with(&config, &windows(&dir), &home(), &[]).unwrap();

        assert_eq!(
            resolved.backends[&id("a")].command,
            dir.path().join("tool.exe")
        );
    }

    #[test]
    fn an_extensionless_file_alone_is_not_found_on_windows() {
        let dir = TempDir::new().unwrap();
        touch(&dir, &["mytool"]);
        let config = config_of(&[("a", entry("mytool", &[]))]);

        let error = resolve_backends_with(&config, &windows(&dir), &home(), &[]).unwrap_err();

        assert!(matches!(error, ConfigError::CommandNotFound { .. }));
    }

    #[test]
    fn resolve_each_reports_a_missing_command_per_backend() {
        let mut config = config_of(&[
            ("found", entry("/opt/agent", &[])),
            ("missing", entry("my-adapter", &[])),
        ]);
        config.default = Some(id("found"));
        let base = pairs(&[("X", "y")]);

        let each = resolve_each(&config, &Lookup::default(), &home(), &base);

        assert_eq!(each.len(), 2);
        let found = each[&id("found")].as_ref().unwrap();
        assert_eq!(found.command, PathBuf::from("/opt/agent"));
        assert_eq!(found.env, base);
        assert!(matches!(
            &each[&id("missing")],
            Err(ConfigError::CommandNotFound { backend, command })
                if *backend == id("missing") && *command == Path::new("my-adapter")
        ));
    }

    #[test]
    fn the_deferred_resolution_names_a_missing_command_and_keeps_the_rest() {
        let mut config = config_of(&[
            ("found", entry("/opt/agent", &[])),
            ("missing", entry("partitur-no-such-adapter", &[])),
        ]);
        config.default = Some(id("found"));
        let base = pairs(&[("PATH", "/nowhere"), ("X", "y")]);

        let resolution = resolution(&config, &base);

        assert_eq!(resolution.len(), 2);
        assert_eq!(
            resolution[&id("found")].as_ref().unwrap().env,
            pairs(&[("PATH", "/nowhere"), ("X", "y")])
        );
        assert_eq!(
            resolution[&id("missing")].as_ref().unwrap_err(),
            "the adapter command partitur-no-such-adapter of backend missing is not on PATH"
        );
    }

    #[test]
    fn the_deferred_resolution_looks_commands_up_on_the_base_path() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("my-adapter"), "").unwrap();
        let mut adapter = entry("my-adapter", &[]);
        adapter.env = BTreeMap::from([("A_HOME".to_owned(), "~".to_owned())]);
        let config = config_of(&[("a", adapter)]);
        let path = dir.path().display().to_string();
        let base = pairs(&[("PATH", &path)]);

        let resolution = resolution(&config, &base);

        let acp = resolution[&id("a")].as_ref().unwrap();
        assert_eq!(acp.command, PathBuf::from("my-adapter"));
        // `~` is the user's home folder, as on a start from a terminal.
        let home = BaseDirs::new().unwrap().home_dir().display().to_string();
        assert_eq!(acp.env, pairs(&[("PATH", &path), ("A_HOME", &home)]));
    }

    #[test]
    fn asking_the_shell_always_defers_the_resolution() {
        let missing = config_of(&[("a", entry("partitur-no-such-adapter", &[]))]);
        let known = ProcessEnv::ready(Vec::new());

        // Even with the environment known already, a start that asks the shell resolves
        // nothing yet, so a missing command cannot stop it.
        assert!(resolve_at_start(&missing, true, &known).unwrap().is_none());
        let (pending, _setter) = ProcessEnv::pending();
        assert!(
            resolve_at_start(&missing, true, &pending)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn without_the_shell_the_backends_resolve_at_the_start() {
        let base = ProcessEnv::ready(pairs(&[("X", "y")]));
        let found = config_of(&[("a", entry("/opt/agent", &[]))]);

        let resolved = resolve_at_start(&found, false, &base).unwrap().unwrap();
        assert_eq!(resolved.backends[&id("a")].env, pairs(&[("X", "y")]));

        let missing = config_of(&[("a", entry("partitur-no-such-adapter", &[]))]);
        assert!(matches!(
            resolve_at_start(&missing, false, &base),
            Err(ConfigError::CommandNotFound { .. })
        ));
    }

    #[test]
    fn listing_names_every_backend_and_the_default() {
        let (_dir, path) = config_file(TWO_BACKENDS);
        let config = Config::load(&path).unwrap();

        let (infos, default) = listing(&config).unwrap();

        assert_eq!(
            infos,
            [
                BackendInfo {
                    id: id("claude-main"),
                    name: "claude-main".into(),
                    command: "claude-agent-acp".into(),
                },
                BackendInfo {
                    id: id("claude-work"),
                    name: "claude-work".into(),
                    command: "/opt/acp/bin/agent".into(),
                },
            ]
        );
        assert_eq!(default, id("claude-main"));
        assert!(matches!(
            listing(&config_of(&[])),
            Err(ConfigError::NoDefault)
        ));
    }

    #[test]
    fn resolving_against_this_process_keeps_a_command_with_a_folder() {
        let resolved =
            resolve_backends(&config_of(&[("a", entry("/opt/missing/agent", &[]))]), &[]).unwrap();

        assert_eq!(
            resolved.backends[&id("a")].command,
            PathBuf::from("/opt/missing/agent")
        );
        assert_eq!(resolved.default, id("a"));
    }
}
