//! The configuration file's format and its loading: the [`Config`] the TOML file deserializes
//! into, with its backends ([`BackendConfig`]) and the window frame ([`FrameConfig`]), and the
//! [`ConfigError`] every step of the configuration reports.
//!
//! [`Config::load`] reads and validates the file, so a loaded configuration always names a
//! default backend it configures; without a file it is [`Config::builtin`]. [`listing`] turns a
//! configuration into the backend list the core and the UI show, and [`window_frame`] into the
//! frame the window is drawn with.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::{fs, io};

use partitur_backend_acp::{ADAPTER_COMMAND, ThinkingDisplay};
use partitur_core::{BackendId, BackendInfo, SettingError};
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
    /// The global pattern for where new worktrees are created; see
    /// [`global_settings`](super::global_settings).
    #[serde(default)]
    pub worktree_path: Option<String>,
    /// The global command clicked links are opened with; see
    /// [`global_settings`](super::global_settings).
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
