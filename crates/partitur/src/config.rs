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
//! A GitHub Copilot CLI backend is an entry like any other, with `copilot --acp` as its
//! command; each profile keeps its own `COPILOT_HOME`, and Claude and Copilot backends mix in
//! one file:
//!
//! ```toml
//! [backends.copilot-main]
//! command = "copilot"
//! args = ["--acp"]
//! env = { COPILOT_HOME = "~/.copilot-main" }
//!
//! [backends.copilot-work]
//! command = "copilot"
//! args = ["--acp"]
//! env = { COPILOT_HOME = "~/.copilot-work", COPILOT_GITHUB_TOKEN = "…" }
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
//! and `npx` when Claude Code is installed. Every backend is then given Partitur's
//! instructions folder in the data folder ([`instructions_dir`], [`instructed`],
//! [`instructed_resolution`]), whose `partitur.instructions.md` agents such as GitHub Copilot
//! CLI read Partitur's tool instructions from.

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

/// The folder under the data folder `data_dir` that Partitur's tool instructions are written
/// to, for agents that read them from a folder.
#[must_use]
pub fn instructions_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("agent-instructions")
}

/// `resolved` with every backend's adapter given the instructions folder `dir`.
#[must_use]
pub fn instructed(resolved: Resolved, dir: &Path) -> Resolved {
    Resolved {
        backends: resolved
            .backends
            .into_iter()
            .map(|(id, acp)| (id, with_instructions(acp, dir)))
            .collect(),
        ..resolved
    }
}

/// `resolution` with every resolved backend's adapter given the instructions folder `dir`; a
/// backend that failed keeps its message.
#[must_use]
pub fn instructed_resolution(resolution: Resolution, dir: &Path) -> Resolution {
    resolution
        .into_iter()
        .map(|(id, result)| (id, result.map(|acp| with_instructions(acp, dir))))
        .collect()
}

/// `acp` with the instructions folder `dir`.
fn with_instructions(acp: AcpConfig, dir: &Path) -> AcpConfig {
    AcpConfig {
        instructions_dir: Some(dir.to_path_buf()),
        ..acp
    }
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
        instructions_dir: None,
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
mod tests;
