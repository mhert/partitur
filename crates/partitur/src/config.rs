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
//! A top-level `window_frame` says who draws the window's title bar: `"partitur"` (the
//! default) makes Partitur's toolbar the title bar, `"system"` keeps the desktop's own above
//! it; any other value stops the start:
//!
//! ```toml
//! window_frame = "system"
//! ```
//!
//! A name must be non-empty and contain no `/` or `\`. `command` is required; `default` may
//! be left out when only one backend is configured.
//!
//! Backends are resolved on top of a process environment, such as the login shell's: its
//! variables come before a backend's `env`, which wins over them, and a bare command is looked
//! up on its `PATH` (else this process's). [`resolve_backends`] stops at the first missing
//! command; [`resolution`] resolves each backend on its own, so a missing command fails only
//! the sessions of its backend. [`resolve_at_start`] picks between them as
//! [`defers_resolution`] decides: whenever the login shell is asked for the environment,
//! resolution waits for its answer, even if it came at once, and on Windows, where a start
//! that stopped could tell nobody, it is always deferred. Both paths take the adapter bundle
//! of the release ([`bundled_adapter`]), which the default adapter command prefers over `PATH`
//! and `npx` when Claude Code is installed.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::{env, fs, io};

use directories::BaseDirs;
use partitur_backend_acp::{
    ADAPTER_COMMAND, ADAPTER_PACKAGE, AcpConfig, Resolution, ThinkingDisplay,
};
use partitur_core::{BackendId, BackendInfo, Level, ProcessEnv, SettingError, SettingsLayer};
use partitur_iced::WindowFrame;
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
    /// Who draws the window's title bar; Partitur's own by default.
    #[serde(default)]
    pub window_frame: Option<FrameConfig>,
}

/// The file's spelling of [`WindowFrame`]: `"partitur"` or `"system"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameConfig {
    /// Partitur's toolbar is the title bar.
    Partitur,
    /// The desktop's own title bar sits above the toolbar.
    System,
}

/// The window frame `config` asks for: `system` keeps the desktop's title bar, anything else
/// (also nothing) is Partitur's own.
#[must_use]
pub fn window_frame(config: &Config) -> WindowFrame {
    match config.window_frame {
        Some(FrameConfig::System) => WindowFrame::System,
        Some(FrameConfig::Partitur) | None => WindowFrame::Partitur,
    }
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
            window_frame: None,
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
/// in lowercase and then as listed: the list is uppercase by default, and the file system there
/// ignores case, so trying `.cmd` first names the found file `npx.cmd`, as npm writes it, where
/// `.CMD` would name it `npx.CMD`.
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
            .flat_map(|ext| [ext.to_lowercase(), ext.clone()])
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
    bundle: Option<&BundledAdapter>,
) -> Result<Resolved, ConfigError> {
    resolve_backends_with(config, &process_lookup(base), &home_folder(), base, bundle)
}

/// Whether the backends are resolved only after the start, with [`resolution`], so a missing
/// command fails only the sessions of its backend: when `shell_asked`, as the login shell's
/// environment comes later, and always when `windows`. There the program has no console, so
/// an error that stopped the start would reach nobody.
#[must_use]
pub fn defers_resolution(shell_asked: bool, windows: bool) -> bool {
    shell_asked || windows
}

/// The backends resolved at the start, or `None` when `deferred` (see [`defers_resolution`]):
/// then the backends are resolved with [`resolution`] once the process environment is known,
/// so a missing command fails only the sessions of its backend instead of the start. That
/// holds even when `env` is known already, as after a shell lookup that failed at once or
/// on Windows. Otherwise `env` is known from the start and this does not wait.
///
/// # Errors
///
/// Returns the errors [`resolve_backends`] does when the resolution is not deferred.
pub fn resolve_at_start(
    config: &Config,
    deferred: bool,
    env: &ProcessEnv,
    bundle: Option<&BundledAdapter>,
) -> Result<Option<Resolved>, ConfigError> {
    if deferred {
        return Ok(None);
    }
    resolve_backends(config, &env.wait(), bundle).map(Some)
}

/// What the adapter runtime starts sessions with once the process environment `base` is known:
/// every backend `config` names, resolved as [`resolve_backends`] does, a backend whose command
/// is missing carrying that error's message instead. Opening a session on such a backend then
/// fails with the message while every other backend works.
pub fn resolution(
    config: &Config,
    base: &[(String, String)],
    bundle: Option<&BundledAdapter>,
) -> Resolution {
    resolve_each(config, &process_lookup(base), &home_folder(), base, bundle)
        .into_iter()
        .map(|(id, result)| (id, result.map_err(|error| error.to_string())))
        .collect()
}

/// The launcher every adapter is started through, the binary's own `current_exe` in its
/// `--job-run` mode, when `windows`: there that ends an adapter's whole process tree when its
/// session stops. Elsewhere, or without a known executable, adapters start directly.
#[must_use]
pub fn launcher(windows: bool, current_exe: Option<PathBuf>) -> Option<PathBuf> {
    current_exe.filter(|_| windows)
}

/// `acp` started through `launcher`: the launcher runs as `launcher --job-run -- <command>
/// <args…>`, with the environment and the thinking display unchanged.
#[must_use]
pub fn with_launcher(acp: AcpConfig, launcher: &Path) -> AcpConfig {
    let args = ["--job-run".to_owned(), "--".to_owned()]
        .into_iter()
        .chain([acp.command.to_string_lossy().into_owned()])
        .chain(acp.args)
        .collect();
    AcpConfig {
        command: launcher.to_path_buf(),
        args,
        ..acp
    }
}

/// `resolved` with every backend started through `launcher`, if there is one; the backend
/// list keeps showing the configured commands.
#[must_use]
pub fn launched(resolved: Resolved, launcher: Option<&Path>) -> Resolved {
    let Some(launcher) = launcher else {
        return resolved;
    };
    Resolved {
        backends: resolved
            .backends
            .into_iter()
            .map(|(id, acp)| (id, with_launcher(acp, launcher)))
            .collect(),
        ..resolved
    }
}

/// `resolution` with every resolved backend started through `launcher`, if there is one; a
/// backend that failed keeps its message.
#[must_use]
pub fn launched_resolution(resolution: Resolution, launcher: Option<&Path>) -> Resolution {
    let Some(launcher) = launcher else {
        return resolution;
    };
    resolution
        .into_iter()
        .map(|(id, result)| (id, result.map(|acp| with_launcher(acp, launcher))))
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
    bundle: Option<&BundledAdapter>,
) -> HashMap<BackendId, Result<AcpConfig, ConfigError>> {
    resolve_in_order(config, lookup, home, base, bundle).collect()
}

/// Every backend `config` names with its resolution, in id order.
fn resolve_in_order<'a>(
    config: &'a Config,
    lookup: &'a Lookup,
    home: &'a Path,
    base: &'a [(String, String)],
    bundle: Option<&'a BundledAdapter>,
) -> impl Iterator<Item = (BackendId, Result<AcpConfig, ConfigError>)> + 'a {
    config.backends.iter().map(move |(id, entry)| {
        (
            id.clone(),
            resolve_entry(id, entry, lookup, home, base, bundle),
        )
    })
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
/// A backend whose command is exactly the default adapter command first tries the `bundle`:
/// when there is one, and both a `node` (the bundled one, else one found by `lookup`) and
/// `claude` are found, it runs `node <bundled entry> <configured args…>` with the
/// `CLAUDE_CODE_EXECUTABLE` variable naming the found `claude` after its environment. Without
/// either, the resolution goes on as above with the `PATH` and then `npx`.
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
    bundle: Option<&BundledAdapter>,
) -> Result<Resolved, ConfigError> {
    let (infos, default) = listing(config)?;
    let backends = resolve_in_order(config, lookup, home, base, bundle)
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
    bundle: Option<&BundledAdapter>,
) -> Result<AcpConfig, ConfigError> {
    let bundled = bundle
        .filter(|_| entry.command == Path::new(ADAPTER_COMMAND))
        .and_then(|bundle| bundled_launch(bundle, lookup));
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
    let (command, args) = if let Some(launch) = &bundled {
        let args = std::iter::once(launch.entry.as_os_str().to_string_lossy().into_owned())
            .chain(entry.args.iter().cloned())
            .collect();
        (launch.node.clone(), args)
    } else if installed {
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
    tracing::info!(
        backend = %id,
        command = %entry.command.display(),
        npx,
        bundled = bundled.is_some(),
        "resolved a backend"
    );
    let claude = bundled.map(|launch| {
        (
            "CLAUDE_CODE_EXECUTABLE".to_owned(),
            launch.claude.to_string_lossy().into_owned(),
        )
    });
    let env = base
        .iter()
        .cloned()
        .chain(
            entry
                .env
                .iter()
                .map(|(name, value)| (name.clone(), expand_home(value, home))),
        )
        .chain(claude)
        .collect();
    Ok(AcpConfig {
        command,
        args,
        env,
        thinking_display: entry.thinking,
    })
}

/// The adapter package a release ships beside the executable, with the Node.js it may bring.
///
/// Found by [`bundled_adapter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundledAdapter {
    /// The adapter's `dist/index.js`.
    pub entry: PathBuf,
    /// The bundled Node.js executable, when the release ships one.
    pub node: Option<PathBuf>,
}

/// The adapter bundle a release ships for the executable `exe`, if it is there.
///
/// The adapter folder sits relative to the executable's folder: `../lib/partitur/adapter` on
/// Linux, `../Resources/adapter` in a macOS bundle (`macos`) and `adapter` on Windows
/// (`windows`). Inside it the entry is `node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js`
/// and the Node.js is `node/bin/node` (`node/node.exe` on Windows) when that file exists. Without
/// the entry file the result is `None`.
#[must_use]
pub fn bundled_adapter(exe: &Path, windows: bool, macos: bool) -> Option<BundledAdapter> {
    let folder = exe.parent()?;
    let adapter = if windows {
        folder.join("adapter")
    } else if macos {
        folder.parent()?.join("Resources/adapter")
    } else {
        folder.parent()?.join("lib/partitur/adapter")
    };
    let entry = adapter.join("node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js");
    if !entry.is_file() {
        return None;
    }
    let node = adapter.join(if windows {
        "node/node.exe"
    } else {
        "node/bin/node"
    });
    let node = node.is_file().then_some(node);
    Some(BundledAdapter { entry, node })
}

/// How the bundled adapter runs: with which Node.js and Claude Code executable.
struct BundledLaunch {
    entry: PathBuf,
    node: PathBuf,
    claude: PathBuf,
}

/// The launch of `bundle`: its own Node.js, else the one `lookup` finds, and the `claude`
/// `lookup` finds. Without either, `None`, with the reason logged.
fn bundled_launch(bundle: &BundledAdapter, lookup: &Lookup) -> Option<BundledLaunch> {
    let Some(node) = bundle
        .node
        .clone()
        .or_else(|| lookup.find(Path::new("node")))
    else {
        tracing::info!("not using the bundled adapter: no Node.js found");
        return None;
    };
    let Some(claude) = lookup.find(Path::new("claude")) else {
        tracing::info!("not using the bundled adapter: no Claude Code found");
        return None;
    };
    Some(BundledLaunch {
        entry: bundle.entry.clone(),
        node,
        claude,
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

    fn parse(content: &str) -> Config {
        toml::from_str(content).unwrap()
    }

    #[test]
    fn the_window_frame_defaults_to_partiturs_own() {
        assert_eq!(window_frame(&parse("")), WindowFrame::Partitur);
    }

    #[test]
    fn the_window_frame_can_be_the_systems() {
        assert_eq!(
            window_frame(&parse(r#"window_frame = "system""#)),
            WindowFrame::System
        );
        assert_eq!(
            window_frame(&parse(r#"window_frame = "partitur""#)),
            WindowFrame::Partitur
        );
    }

    #[test]
    fn an_unknown_window_frame_is_an_error() {
        assert!(toml::from_str::<Config>(r#"window_frame = "fancy""#).is_err());
    }

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
            window_frame: None,
            backends: entries
                .iter()
                .map(|(name, entry)| (id(name), entry.clone()))
                .collect(),
        }
    }

    fn home() -> PathBuf {
        PathBuf::from("/home/me")
    }

    /// `rest` under [`home`], joined as the expansion joins it: with `\` on Windows.
    fn under_home(rest: &str) -> String {
        home().join(rest).display().to_string()
    }

    /// `folders` as one `PATH` value, with the platform's separator.
    fn path_of(folders: &[&str]) -> String {
        env::join_paths(folders).unwrap().into_string().unwrap()
    }

    #[test]
    fn the_launcher_wraps_the_adapter_command() {
        let acp = AcpConfig {
            command: PathBuf::from("/n/npx.cmd"),
            args: vec!["--yes".to_owned(), "pkg".to_owned()],
            env: vec![("CLAUDE_CONFIG_DIR".to_owned(), "/c".to_owned())],
            thinking_display: ThinkingDisplay::Omitted,
        };

        let wrapped = with_launcher(acp, Path::new("/l/partitur.exe"));

        assert_eq!(wrapped.command, PathBuf::from("/l/partitur.exe"));
        assert_eq!(
            wrapped.args,
            ["--job-run", "--", "/n/npx.cmd", "--yes", "pkg"]
        );
        assert_eq!(
            wrapped.env,
            [("CLAUDE_CONFIG_DIR".to_owned(), "/c".to_owned())]
        );
        assert_eq!(wrapped.thinking_display, ThinkingDisplay::Omitted);
    }

    #[test]
    fn only_windows_runs_adapters_through_the_launcher() {
        let exe = PathBuf::from("/l/partitur.exe");
        assert_eq!(launcher(true, Some(exe.clone())), Some(exe.clone()));
        assert_eq!(launcher(false, Some(exe)), None);
        assert_eq!(launcher(true, None), None);
    }

    fn acp(command: &str) -> AcpConfig {
        AcpConfig {
            command: PathBuf::from(command),
            args: vec!["a".to_owned()],
            env: Vec::new(),
            thinking_display: ThinkingDisplay::Summarized,
        }
    }

    /// The command and arguments `acp` starts.
    fn command_line(acp: &AcpConfig) -> (PathBuf, Vec<String>) {
        (acp.command.clone(), acp.args.clone())
    }

    fn wrapped_line(command: &str) -> (PathBuf, Vec<String>) {
        (
            PathBuf::from("/l/partitur.exe"),
            ["--job-run", "--", command, "a"]
                .map(str::to_owned)
                .to_vec(),
        )
    }

    #[test]
    fn a_launcher_wraps_every_backend_resolved_at_the_start_but_not_its_listing() {
        let resolved = || Resolved {
            backends: HashMap::from([(id("a"), acp("/x/a")), (id("b"), acp("/x/b"))]),
            infos: listing(&config_of(&[("a", entry("/x/a", &[]))])).unwrap().0,
            default: id("a"),
        };

        let wrapped = launched(resolved(), Some(Path::new("/l/partitur.exe")));

        assert_eq!(
            command_line(&wrapped.backends[&id("a")]),
            wrapped_line("/x/a")
        );
        assert_eq!(
            command_line(&wrapped.backends[&id("b")]),
            wrapped_line("/x/b")
        );
        assert_eq!(wrapped.backends.len(), 2);
        assert_eq!(wrapped.infos, resolved().infos);
        assert_eq!(wrapped.default, id("a"));
        let unwrapped = launched(resolved(), None);
        assert_eq!(
            command_line(&unwrapped.backends[&id("a")]),
            command_line(&acp("/x/a"))
        );
        assert_eq!(unwrapped.backends.len(), 2);
    }

    #[test]
    fn a_launcher_wraps_every_backend_of_a_resolution_and_keeps_its_errors() {
        let resolution = || -> Resolution {
            HashMap::from([
                (id("a"), Ok(acp("/x/a"))),
                (id("b"), Err("missing".to_owned())),
            ])
        };

        let wrapped = launched_resolution(resolution(), Some(Path::new("/l/partitur.exe")));

        assert_eq!(
            wrapped[&id("a")].as_ref().map(command_line),
            Ok(wrapped_line("/x/a"))
        );
        assert_eq!(
            wrapped[&id("b")].as_ref().err(),
            Some(&"missing".to_owned())
        );
        assert_eq!(wrapped.len(), 2);
        let unwrapped = launched_resolution(resolution(), None);
        assert_eq!(
            unwrapped[&id("a")].as_ref().map(command_line),
            Ok(command_line(&acp("/x/a")))
        );
        assert_eq!(
            unwrapped[&id("b")].as_ref().err(),
            Some(&"missing".to_owned())
        );
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
        let resolved =
            resolve_backends_with(&config, &Lookup::default(), &home(), &[], None).unwrap();
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
            None,
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
            None,
        )
        .unwrap();

        let (x, nested, bin) = (under_home("x"), under_home("a/~/b"), under_home("bin"));
        let expected: Vec<(String, String)> = [
            ("A_DIR", x.as_str()),
            ("B_HOME", "/home/me"),
            ("C_PATH", "/opt/~x"),
            ("D_USER", "~user"),
            ("E_NESTED", nested.as_str()),
            ("F_SLASHES", x.as_str()),
            ("G_BACKSLASH", bin.as_str()),
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
            None,
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
            None,
        )
        .unwrap();
        let acp = &resolved.backends[&id("main")];
        assert_eq!(acp.command, PathBuf::from("npx"));
        assert_eq!(
            acp.args,
            [
                "--yes",
                "@agentclientprotocol/claude-agent-acp@0.108.0",
                "-v"
            ]
        );
        assert_eq!(acp.thinking_display, ThinkingDisplay::Omitted);
        // The list shows what the user configured, not the fallback.
        assert_eq!(resolved.infos[0].command, "claude-agent-acp");

        let mut both = config_of(&[("main", fallback), ("custom", entry("my-adapter", &[]))]);
        both.default = Some(id("main"));
        let error = resolve_backends_with(&both, &plain(&path), &home(), &[], None).unwrap_err();
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

        let resolved = resolve_backends_with(&config, &plain(&path), &home(), &[], None).unwrap();

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
                None,
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

        let resolved =
            resolve_backends_with(&config, &Lookup::default(), &home(), &[], None).unwrap();

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
                ("CLAUDE_CONFIG_DIR".to_owned(), under_home(".claude-work")),
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
                resolve_backends_with(&config, &Lookup::default(), &home(), &[], None).unwrap_err();

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
            resolve_backends_with(&config, &Lookup::default(), &home(), &[], None).unwrap();
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
            None,
        )
        .unwrap();

        // Later entries win, so a configured variable overrides the shell's of the same name.
        assert_eq!(
            resolved.backends[&id("work")].env,
            pairs(&[
                ("PATH", "/shell/bin"),
                ("Z_TOKEN", "t"),
                ("A_DIR", &under_home("a")),
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
        let (first, last) = (path_of(&["/a", "/b"]), path_of(&["/c", "/d"]));
        let base = pairs(&[("HOME", "/h"), ("PATH", &first), ("PATH", &last)]);

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
            Lookup::from_env(&base, Some(path_of(&["/p", "/q"]).into()), None, false).folders,
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

        let resolved = resolve_backends_with(&config, &windows(&dir), &home(), &[], None).unwrap();

        let acp = &resolved.backends[&id("claude")];
        assert_eq!(acp.command, dir.path().join("npx.cmd"));
        assert_eq!(
            acp.args,
            [
                "--yes",
                "@agentclientprotocol/claude-agent-acp@0.108.0",
                "-v"
            ]
        );
    }

    #[test]
    fn without_npx_the_windows_fallback_stays_the_bare_name() {
        let dir = TempDir::new().unwrap();
        touch(&dir, &["npx"]);
        let config = config_of(&[("claude", entry("claude-agent-acp", &[]))]);

        let resolved = resolve_backends_with(&config, &windows(&dir), &home(), &[], None).unwrap();

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

        let resolved = resolve_backends_with(&config, &windows(&dir), &home(), &[], None).unwrap();

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
    fn the_lowercase_extension_is_tried_before_the_listed_one() {
        let dir = TempDir::new().unwrap();
        // On a file system that ignores case both names are the one file, and the found path
        // is spelled as it was tried; here they are two, so the order shows.
        touch(&dir, &["tool.CMD", "tool.cmd"]);
        let lookup = Lookup {
            folders: vec![dir.path().to_path_buf()],
            extensions: vec![".CMD".to_owned()],
            full_path: true,
        };

        assert_eq!(
            lookup.find(Path::new("tool")),
            Some(dir.path().join("tool.cmd"))
        );
    }

    #[test]
    fn a_command_with_an_extension_is_tried_as_written() {
        let dir = TempDir::new().unwrap();
        touch(&dir, &["tool.exe"]);
        let config = config_of(&[("a", entry("tool.exe", &[]))]);

        let resolved = resolve_backends_with(&config, &windows(&dir), &home(), &[], None).unwrap();

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

        let error = resolve_backends_with(&config, &windows(&dir), &home(), &[], None).unwrap_err();

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

        let each = resolve_each(&config, &Lookup::default(), &home(), &base, None);

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

        let resolution = resolution(&config, &base, None);

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
        // Windows finds a bare name only with an extension, and runs it by its full path.
        let file = if cfg!(windows) {
            "my-adapter.cmd"
        } else {
            "my-adapter"
        };
        fs::write(dir.path().join(file), "").unwrap();
        let mut adapter = entry("my-adapter", &[]);
        adapter.env = BTreeMap::from([("A_HOME".to_owned(), "~".to_owned())]);
        let config = config_of(&[("a", adapter)]);
        let path = dir.path().display().to_string();
        let base = pairs(&[("PATH", &path)]);

        let resolution = resolution(&config, &base, None);

        let acp = resolution[&id("a")].as_ref().unwrap();
        let command = if cfg!(windows) {
            dir.path().join(file)
        } else {
            PathBuf::from(file)
        };
        assert_eq!(acp.command, command);
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
        assert!(
            resolve_at_start(&missing, true, &known, None)
                .unwrap()
                .is_none()
        );
        let (pending, _setter) = ProcessEnv::pending();
        assert!(
            resolve_at_start(&missing, true, &pending, None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn the_resolution_is_deferred_when_the_shell_is_asked_or_on_windows() {
        assert!(!defers_resolution(false, false));
        assert!(defers_resolution(true, false));
        assert!(defers_resolution(false, true));
        assert!(defers_resolution(true, true));
    }

    #[test]
    fn without_the_shell_the_backends_resolve_at_the_start() {
        let base = ProcessEnv::ready(pairs(&[("X", "y")]));
        let found = config_of(&[("a", entry("/opt/agent", &[]))]);

        let resolved = resolve_at_start(&found, false, &base, None)
            .unwrap()
            .unwrap();
        assert_eq!(resolved.backends[&id("a")].env, pairs(&[("X", "y")]));

        let missing = config_of(&[("a", entry("partitur-no-such-adapter", &[]))]);
        assert!(matches!(
            resolve_at_start(&missing, false, &base, None),
            Err(ConfigError::CommandNotFound { .. })
        ));
    }

    const ENTRY_FILE: &str = "node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js";

    /// Writes the empty files `names` under `root`, with their folders.
    fn lay(root: &Path, names: &[&str]) {
        for name in names {
            let file = root.join(name);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, "").unwrap();
        }
    }

    /// The default adapter command as the only backend.
    fn default_adapter(args: &[&str]) -> Config {
        config_of(&[("main", entry(ADAPTER_COMMAND, args))])
    }

    /// A bundle at `root` with the entry file, and `node` as its own Node.js when given.
    fn bundle_in(root: &Path, node: Option<&str>) -> BundledAdapter {
        lay(root, &[ENTRY_FILE]);
        let node = node.map(|name| {
            lay(root, &[name]);
            root.join(name)
        });
        BundledAdapter {
            entry: root.join(ENTRY_FILE),
            node,
        }
    }

    /// Asserts that both resolve `main` to the same command, arguments and environment.
    fn assert_same(a: &Resolved, b: &Resolved) {
        let (a, b) = (&a.backends[&id("main")], &b.backends[&id("main")]);
        assert_eq!(a.command, b.command);
        assert_eq!(a.args, b.args);
        assert_eq!(a.env, b.env);
    }

    #[test]
    fn bundled_adapter_found_beside_linux_exe() {
        let tmp = TempDir::new().unwrap();
        let adapter = "usr/lib/partitur/adapter";
        lay(
            tmp.path(),
            &[&format!("{adapter}/{ENTRY_FILE}"), "usr/bin/partitur"],
        );

        let found = bundled_adapter(&tmp.path().join("usr/bin/partitur"), false, false).unwrap();

        assert!(
            found
                .entry
                .ends_with(format!("usr/lib/partitur/adapter/{ENTRY_FILE}"))
        );
        assert!(found.entry.is_file());
        assert_eq!(found.node, None);

        lay(tmp.path(), &[&format!("{adapter}/node/bin/node")]);
        let found = bundled_adapter(&tmp.path().join("usr/bin/partitur"), false, false).unwrap();
        assert!(found.node.unwrap().ends_with("adapter/node/bin/node"));
    }

    #[test]
    fn bundled_adapter_found_in_macos_resources() {
        let tmp = TempDir::new().unwrap();
        lay(
            tmp.path(),
            &[
                &format!("Contents/Resources/adapter/{ENTRY_FILE}"),
                "Contents/MacOS/partitur",
                "Contents/Resources/adapter/node/bin/node",
            ],
        );
        let exe = tmp.path().join("Contents/MacOS/partitur");

        let found = bundled_adapter(&exe, false, true).unwrap();

        assert!(
            found
                .entry
                .ends_with(format!("Contents/Resources/adapter/{ENTRY_FILE}"))
        );
        assert!(
            found
                .node
                .unwrap()
                .ends_with("Resources/adapter/node/bin/node")
        );
        // The same layout is no Linux bundle.
        assert_eq!(bundled_adapter(&exe, false, false), None);
    }

    #[test]
    fn bundled_adapter_found_beside_windows_exe() {
        let tmp = TempDir::new().unwrap();
        lay(
            tmp.path(),
            &[
                &format!("adapter/{ENTRY_FILE}"),
                "adapter/node/node.exe",
                "adapter/node/bin/node",
            ],
        );
        let exe = tmp.path().join("partitur.exe");

        let found = bundled_adapter(&exe, true, false).unwrap();

        assert!(found.entry.ends_with(format!("adapter/{ENTRY_FILE}")));
        assert!(found.node.unwrap().ends_with("adapter/node/node.exe"));
        assert_eq!(bundled_adapter(&exe, false, false), None);
    }

    #[test]
    fn bundled_adapter_missing_entry_is_none() {
        let tmp = TempDir::new().unwrap();
        lay(
            tmp.path(),
            &["usr/lib/partitur/adapter/node/bin/node", "usr/bin/partitur"],
        );

        assert_eq!(
            bundled_adapter(&tmp.path().join("usr/bin/partitur"), false, false),
            None
        );
        assert_eq!(bundled_adapter(Path::new("/"), false, false), None);
    }

    #[test]
    fn bundled_adapter_wins_over_path_and_npx() {
        let tmp = TempDir::new().unwrap();
        let bin = TempDir::new().unwrap();
        touch(&bin, &["claude", "claude-agent-acp", "node"]);
        let bundle = bundle_in(tmp.path(), Some("own-node"));

        let resolved = resolve_backends_with(
            &default_adapter(&["-v"]),
            &plain(&[bin.path().to_path_buf()]),
            &home(),
            &pairs(&[("X", "y")]),
            Some(&bundle),
        )
        .unwrap();

        let acp = &resolved.backends[&id("main")];
        assert_eq!(acp.command, tmp.path().join("own-node"));
        assert_eq!(
            acp.args,
            [bundle.entry.display().to_string(), "-v".to_owned()]
        );
        assert_eq!(
            acp.env,
            pairs(&[
                ("X", "y"),
                (
                    "CLAUDE_CODE_EXECUTABLE",
                    &bin.path().join("claude").display().to_string()
                ),
            ])
        );
    }

    #[test]
    fn bundled_adapter_keeps_the_configured_env_before_the_claude_variable() {
        let tmp = TempDir::new().unwrap();
        let bin = TempDir::new().unwrap();
        touch(&bin, &["claude"]);
        let bundle = bundle_in(tmp.path(), Some("own-node"));
        let mut configured = entry(ADAPTER_COMMAND, &[]);
        configured.env.insert("A".to_owned(), "b".to_owned());

        let resolved = resolve_backends_with(
            &config_of(&[("main", configured)]),
            &plain(&[bin.path().to_path_buf()]),
            &home(),
            &pairs(&[("X", "y")]),
            Some(&bundle),
        )
        .unwrap();

        let env = &resolved.backends[&id("main")].env;
        assert_eq!(env[..2], pairs(&[("X", "y"), ("A", "b")]));
        assert_eq!(env[2].0, "CLAUDE_CODE_EXECUTABLE");
    }

    #[test]
    fn bundled_adapter_uses_system_node_without_bundled_node() {
        let tmp = TempDir::new().unwrap();
        let bin = TempDir::new().unwrap();
        touch(&bin, &["claude", "node"]);
        let bundle = bundle_in(tmp.path(), None);

        let resolved = resolve_backends_with(
            &default_adapter(&[]),
            &plain(&[bin.path().to_path_buf()]),
            &home(),
            &[],
            Some(&bundle),
        )
        .unwrap();

        let acp = &resolved.backends[&id("main")];
        assert_eq!(acp.command, bin.path().join("node"));
        assert_eq!(acp.args, [bundle.entry.display().to_string()]);
    }

    #[test]
    fn bundled_adapter_skipped_without_claude() {
        let tmp = TempDir::new().unwrap();
        let bin = TempDir::new().unwrap();
        touch(&bin, &["node"]);
        let bundle = bundle_in(tmp.path(), Some("own-node"));
        let lookup = plain(&[bin.path().to_path_buf()]);
        let config = default_adapter(&["-v"]);

        let with = resolve_backends_with(&config, &lookup, &home(), &[], Some(&bundle)).unwrap();
        let without = resolve_backends_with(&config, &lookup, &home(), &[], None).unwrap();

        assert_same(&with, &without);
        assert_eq!(with.backends[&id("main")].command, PathBuf::from("npx"));
    }

    #[test]
    fn bundled_adapter_skipped_without_any_node() {
        let tmp = TempDir::new().unwrap();
        let bin = TempDir::new().unwrap();
        touch(&bin, &["claude"]);
        let bundle = bundle_in(tmp.path(), None);
        let lookup = plain(&[bin.path().to_path_buf()]);
        let config = default_adapter(&[]);

        let with = resolve_backends_with(&config, &lookup, &home(), &[], Some(&bundle)).unwrap();
        let without = resolve_backends_with(&config, &lookup, &home(), &[], None).unwrap();

        assert_same(&with, &without);
    }

    #[test]
    fn custom_command_ignores_bundle() {
        let tmp = TempDir::new().unwrap();
        let bin = TempDir::new().unwrap();
        touch(&bin, &["claude", "node"]);
        let bundle = bundle_in(tmp.path(), Some("own-node"));

        let resolved = resolve_backends_with(
            &config_of(&[("main", entry("/opt/my-acp", &["-x"]))]),
            &plain(&[bin.path().to_path_buf()]),
            &home(),
            &[],
            Some(&bundle),
        )
        .unwrap();

        let acp = &resolved.backends[&id("main")];
        assert_eq!(acp.command, PathBuf::from("/opt/my-acp"));
        assert_eq!(acp.args, ["-x"]);
        assert!(acp.env.is_empty());
    }

    #[test]
    fn the_deferred_resolution_uses_the_bundle_too() {
        let tmp = TempDir::new().unwrap();
        let bin = TempDir::new().unwrap();
        touch(&bin, &["claude"]);
        let bundle = bundle_in(tmp.path(), Some("own-node"));
        let path = bin.path().display().to_string();
        let base = pairs(&[("PATH", &path)]);

        let deferred = resolution(&default_adapter(&[]), &base, Some(&bundle));

        let acp = deferred[&id("main")].as_ref().unwrap();
        assert_eq!(acp.command, tmp.path().join("own-node"));
        assert_eq!(acp.args, [bundle.entry.display().to_string()]);

        let start = ProcessEnv::ready(base);
        let resolved = resolve_at_start(&default_adapter(&[]), false, &start, Some(&bundle))
            .unwrap()
            .unwrap();
        assert_eq!(
            resolved.backends[&id("main")].command,
            tmp.path().join("own-node")
        );
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
        let resolved = resolve_backends(
            &config_of(&[("a", entry("/opt/missing/agent", &[]))]),
            &[],
            None,
        )
        .unwrap();

        assert_eq!(
            resolved.backends[&id("a")].command,
            PathBuf::from("/opt/missing/agent")
        );
        assert_eq!(resolved.default, id("a"));
    }
}
