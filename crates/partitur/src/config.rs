//! The user's configuration file and how it decides which agent adapter runs.
//!
//! The file is TOML; every key is optional:
//!
//! ```toml
//! [backend]
//! command = "claude-agent-acp"   # the ACP adapter; a bare name is looked up on PATH
//! args = []
//! thinking = "summarized"        # or "omitted"
//! ```

use std::path::{Path, PathBuf};
use std::{env, fs, io};

use partitur_backend_acp::{ADAPTER_COMMAND, ADAPTER_PACKAGE, AcpConfig, ThinkingDisplay};
use serde::Deserialize;

/// Everything the configuration file sets.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// How the agent adapter is started.
    pub backend: BackendConfig,
}

/// How the agent adapter is started.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BackendConfig {
    /// The adapter program; `claude-agent-acp` by default.
    pub command: PathBuf,
    /// The adapter's arguments; none by default.
    pub args: Vec<String>,
    /// How the agent's reasoning is reported; summarized by default.
    #[serde(with = "ThinkingDisplayDef")]
    pub thinking: ThinkingDisplay,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            command: PathBuf::from(ADAPTER_COMMAND),
            args: Vec::new(),
            thinking: ThinkingDisplay::Summarized,
        }
    }
}

/// The file's spelling of [`ThinkingDisplay`]: `"summarized"` or `"omitted"`.
#[derive(Deserialize)]
#[serde(remote = "ThinkingDisplay", rename_all = "lowercase")]
enum ThinkingDisplayDef {
    Summarized,
    Omitted,
}

/// Why the configuration file could not be used.
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
    /// The configured adapter command is a bare name found in no folder of `PATH`.
    #[error("the adapter command {} is not on PATH", command.display())]
    CommandNotFound {
        /// The configured command.
        command: PathBuf,
    },
}

impl Config {
    /// Reads the configuration at `path`; a file that does not exist yields the defaults.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Io`] when the file exists but cannot be read and
    /// [`ConfigError::Parse`] when it is not valid TOML, has a key Partitur does not know or
    /// a value of the wrong kind.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }
}

/// How to start the adapter `config` names, looked up on this process's `PATH`.
///
/// See [`resolve_backend_with`].
///
/// # Errors
///
/// Returns [`ConfigError::CommandNotFound`] as [`resolve_backend_with`] does.
pub fn resolve_backend(config: &BackendConfig) -> Result<AcpConfig, ConfigError> {
    let path: Vec<PathBuf> = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect())
        .unwrap_or_default();
    resolve_backend_with(config, &path)
}

/// How to start the adapter `config` names, looked up in the folders of `path`.
///
/// A command with a folder in it runs as configured, as does a bare name found as a file in
/// one of the folders. The default adapter command found nowhere falls back to running the
/// pinned Claude Code adapter package through `npx`, with the configured arguments after the
/// package name; the fallback is logged.
///
/// # Errors
///
/// Returns [`ConfigError::CommandNotFound`] when any other bare name is found nowhere.
pub fn resolve_backend_with(
    config: &BackendConfig,
    path: &[PathBuf],
) -> Result<AcpConfig, ConfigError> {
    let has_folder = config
        .command
        .parent()
        .is_some_and(|folder| !folder.as_os_str().is_empty());
    let installed = has_folder || path.iter().any(|dir| dir.join(&config.command).is_file());
    let (command, args) = if installed {
        (config.command.clone(), config.args.clone())
    } else if config.command == Path::new(ADAPTER_COMMAND) {
        let command = config.command.display();
        tracing::info!(%command, "the adapter is not on PATH, running it through npx");
        let args = ["--yes", ADAPTER_PACKAGE]
            .into_iter()
            .map(str::to_owned)
            .chain(config.args.iter().cloned())
            .collect();
        (PathBuf::from("npx"), args)
    } else {
        return Err(ConfigError::CommandNotFound {
            command: config.command.clone(),
        });
    };
    Ok(AcpConfig {
        command,
        args,
        env: Vec::new(),
        thinking_display: config.thinking,
    })
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

    fn config_file(content: &str) -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, content).unwrap();
        (dir, path)
    }

    fn backend(command: &str, args: &[&str]) -> BackendConfig {
        BackendConfig {
            command: PathBuf::from(command),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            thinking: ThinkingDisplay::Omitted,
        }
    }

    #[test]
    fn a_missing_config_yields_defaults() {
        let dir = TempDir::new().unwrap();

        let config = Config::load(&dir.path().join("config.toml")).unwrap();

        assert_eq!(config, Config::default());
        assert_eq!(config.backend.command, PathBuf::from("claude-agent-acp"));
        assert!(config.backend.args.is_empty());
        assert_eq!(config.backend.thinking, ThinkingDisplay::Summarized);
    }

    #[test]
    fn a_config_file_overrides_command_and_thinking() {
        let (_dir, path) = config_file(
            "[backend]\ncommand = \"/opt/acp/bin/agent\"\nargs = [\"--verbose\"]\nthinking = \"omitted\"\n",
        );

        let config = Config::load(&path).unwrap();

        assert_eq!(
            config.backend,
            backend("/opt/acp/bin/agent", &["--verbose"])
        );
    }

    #[test]
    fn keys_left_out_keep_their_defaults() {
        let (_dir, path) = config_file("[backend]\nthinking = \"omitted\"\n");

        let config = Config::load(&path).unwrap();

        assert_eq!(config.backend.command, PathBuf::from("claude-agent-acp"));
        assert!(config.backend.args.is_empty());
        assert_eq!(config.backend.thinking, ThinkingDisplay::Omitted);
        assert_eq!(Config::load(&config_file("").1).unwrap(), Config::default());
    }

    #[test]
    fn malformed_toml_is_a_parse_error_with_the_path() {
        let (_dir, path) = config_file("[backend\ncommand = ");

        let error = Config::load(&path).unwrap_err();

        assert!(matches!(&error, ConfigError::Parse { path: named, .. } if *named == path));
        assert!(error.to_string().contains(&path.display().to_string()));
    }

    #[test]
    fn an_unknown_key_or_thinking_value_is_a_parse_error() {
        for content in [
            "[backend]\ncomand = \"agent\"\n",
            "[backend]\nthinking = \"verbose\"\n",
        ] {
            let (_dir, path) = config_file(content);

            let error = Config::load(&path).unwrap_err();

            assert!(matches!(error, ConfigError::Parse { .. }), "{content}");
        }
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
    fn a_command_found_on_the_path_runs_as_configured() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("claude-agent-acp"), "").unwrap();
        let path = [PathBuf::from("/nowhere"), dir.path().to_path_buf()];

        let acp = resolve_backend_with(&backend("claude-agent-acp", &["-v"]), &path).unwrap();

        assert_eq!(acp.command, PathBuf::from("claude-agent-acp"));
        assert_eq!(acp.args, ["-v"]);
        assert!(acp.env.is_empty());
        assert_eq!(acp.thinking_display, ThinkingDisplay::Omitted);
    }

    #[test]
    fn a_bare_command_missing_from_the_path_runs_the_adapter_through_npx() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("claude-agent-acp")).unwrap();

        let acp = resolve_backend_with(
            &backend("claude-agent-acp", &["-v"]),
            &[dir.path().to_path_buf()],
        )
        .unwrap();

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
    }

    #[test]
    fn a_missing_custom_command_is_an_error_not_a_fallback() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("my-adapter")).unwrap();

        let error = resolve_backend_with(&backend("my-adapter", &[]), &[dir.path().to_path_buf()])
            .unwrap_err();

        assert!(
            matches!(&error, ConfigError::CommandNotFound { command } if *command == Path::new("my-adapter"))
        );
        assert_eq!(
            error.to_string(),
            "the adapter command my-adapter is not on PATH"
        );
    }

    #[test]
    fn the_npx_fallback_is_logged_at_info() {
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("log");
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .with_writer(Arc::new(fs::File::create(&log).unwrap()))
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            resolve_backend_with(&backend("claude-agent-acp", &[]), &[]).unwrap();
        });

        let written = fs::read_to_string(&log).unwrap();
        assert!(written.contains("INFO"), "{written}");
        assert!(written.contains("command=claude-agent-acp"), "{written}");
        assert!(written.contains("npx"), "{written}");
    }

    #[test]
    fn a_command_with_a_folder_runs_as_configured_even_when_missing() {
        for command in ["/opt/missing/agent", "bin/agent"] {
            let acp = resolve_backend_with(&backend(command, &[]), &[]).unwrap();

            assert_eq!(acp.command, PathBuf::from(command));
            assert!(acp.args.is_empty());
        }
    }

    #[test]
    fn resolving_against_this_process_keeps_a_command_with_a_folder() {
        let acp = resolve_backend(&backend("/opt/missing/agent", &[])).unwrap();

        assert_eq!(acp.command, PathBuf::from("/opt/missing/agent"));
    }
}
