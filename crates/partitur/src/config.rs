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
//! A name must be non-empty and contain no `/` or `\`. `command` is required; `default` may
//! be left out when only one backend is configured.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::{env, fs, io};

use directories::BaseDirs;
use partitur_backend_acp::{ADAPTER_COMMAND, ADAPTER_PACKAGE, AcpConfig, ThinkingDisplay};
use partitur_core::{BackendId, BackendInfo};
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

/// Resolves every backend `config` names, looking commands up on this process's `PATH` and
/// expanding `~` to the user's home folder.
///
/// See [`resolve_backends_with`].
///
/// # Errors
///
/// Returns the errors [`resolve_backends_with`] does.
pub fn resolve_backends(config: &Config) -> Result<Resolved, ConfigError> {
    let path: Vec<PathBuf> = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect())
        .unwrap_or_default();
    // Without a home folder a `~` stays as written.
    let home = BaseDirs::new().map_or_else(|| PathBuf::from("~"), |dirs| dirs.home_dir().into());
    resolve_backends_with(config, &path, &home)
}

/// Resolves every backend `config` names, looking commands up in the folders of
/// `path_entries` and expanding `~` to `home`.
///
/// A command with a folder in it runs as configured, as does a bare name found as a file in
/// one of the folders. The default adapter command found nowhere falls back to running the
/// pinned Claude Code adapter package through `npx`, with the configured arguments after the
/// package name. Each backend's `env` is passed on in name order, a value of `~` or starting
/// with `~/` having that `~` replaced by `home`; the adapter's transport sets the thinking
/// variable after them. Each resolved backend is logged with its command and whether the
/// fallback applied.
///
/// The backend list shows each backend's command as configured, even when it runs through
/// `npx`.
///
/// # Errors
///
/// Returns [`ConfigError::CommandNotFound`] when a bare name other than the default adapter
/// command is found nowhere, and [`ConfigError::NoDefault`] when `config` names no default
/// it configures (which a loaded configuration always does).
pub fn resolve_backends_with(
    config: &Config,
    path_entries: &[PathBuf],
    home: &Path,
) -> Result<Resolved, ConfigError> {
    let default = config.effective_default().ok_or(ConfigError::NoDefault)?;
    let mut backends = HashMap::new();
    let mut infos = Vec::new();
    for (id, entry) in &config.backends {
        backends.insert(id.clone(), resolve_entry(id, entry, path_entries, home)?);
        infos.push(BackendInfo {
            id: id.clone(),
            name: id.to_string(),
            command: entry.command.display().to_string(),
        });
    }
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
    path_entries: &[PathBuf],
    home: &Path,
) -> Result<AcpConfig, ConfigError> {
    let has_folder = entry
        .command
        .parent()
        .is_some_and(|folder| !folder.as_os_str().is_empty());
    let installed = has_folder
        || path_entries
            .iter()
            .any(|dir| dir.join(&entry.command).is_file());
    let npx = !installed && entry.command == Path::new(ADAPTER_COMMAND);
    let (command, args) = if installed {
        (entry.command.clone(), entry.args.clone())
    } else if npx {
        let args = ["--yes", ADAPTER_PACKAGE]
            .into_iter()
            .map(str::to_owned)
            .chain(entry.args.iter().cloned())
            .collect();
        (PathBuf::from("npx"), args)
    } else {
        return Err(ConfigError::CommandNotFound {
            backend: id.clone(),
            command: entry.command.clone(),
        });
    };
    tracing::info!(backend = %id, command = %entry.command.display(), npx, "resolved a backend");
    let env = entry
        .env
        .iter()
        .map(|(name, value)| (name.clone(), expand_home(value, home)))
        .collect();
    Ok(AcpConfig {
        command,
        args,
        env,
        thinking_display: entry.thinking,
    })
}

/// `value` with a leading `~` (alone or followed by `/`) replaced by `home`; any other value
/// unchanged. Extra slashes after the `~` are dropped, so the result stays under `home`
/// (joining an absolute rest would replace `home` altogether).
fn expand_home(value: &str, home: &Path) -> String {
    if value == "~" {
        home.display().to_string()
    } else if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest.trim_start_matches('/'))
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
        let resolved = resolve_backends_with(&config, &[], &home()).unwrap();
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
        let resolved = resolve_backends_with(&Config::load(&path).unwrap(), &[], &home()).unwrap();
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
        ]);

        let resolved = resolve_backends_with(&config_of(&[("work", work)]), &[], &home()).unwrap();

        let expected: Vec<(String, String)> = [
            ("A_DIR", "/home/me/x"),
            ("B_HOME", "/home/me"),
            ("C_PATH", "/opt/~x"),
            ("D_USER", "~user"),
            ("E_NESTED", "/home/me/a/~/b"),
            ("F_SLASHES", "/home/me/x"),
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

        let resolved = resolve_backends_with(&config_of(&[("work", work)]), &[], &home()).unwrap();

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

        let resolved =
            resolve_backends_with(&config_of(&[("main", fallback.clone())]), &path, &home())
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
        let error = resolve_backends_with(&both, &path, &home()).unwrap_err();
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

        let resolved = resolve_backends_with(&config, &path, &home()).unwrap();

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
            let resolved =
                resolve_backends_with(&config_of(&[("a", entry(command, &[]))]), &[], &home())
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

        let resolved = resolve_backends_with(&config, &[], &home()).unwrap();

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
            let error = resolve_backends_with(&config, &[], &home()).unwrap_err();

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
            resolve_backends_with(&config, &[], &home()).unwrap();
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

    #[test]
    fn resolving_against_this_process_keeps_a_command_with_a_folder() {
        let resolved =
            resolve_backends(&config_of(&[("a", entry("/opt/missing/agent", &[]))])).unwrap();

        assert_eq!(
            resolved.backends[&id("a")].command,
            PathBuf::from("/opt/missing/agent")
        );
        assert_eq!(resolved.default, id("a"));
    }
}
