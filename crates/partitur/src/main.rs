//! The Partitur desktop application: wires the store, the agent backend, the orchestrator tool
//! server and the GUI together.
//!
//! - The configuration is `config.toml` in the platform's config folder (on Linux
//!   `~/.config/partitur/config.toml`); the [`config`] module lists its keys. It names the
//!   agent backends and the default one new sessions run on. Without the file one backend,
//!   `claude`, runs the Claude Code adapter `claude-agent-acp`. A backend whose command is
//!   `claude-agent-acp` runs it through `npx` (pinned to the version Partitur was tested
//!   with) when it is not installed. Any other configured command must be installed; a
//!   missing one stops the start with an error naming its backend.
//! - Projects, sessions and transcripts are kept in the platform's data folder (on Linux
//!   `~/.local/share/partitur`).
//! - `RUST_LOG` filters the log written to stderr; the default is `partitur=info`.
//! - `PARTITUR_OPEN=<folder>` opens that folder as a project at startup, as if it had been
//!   picked in the folder dialog.

mod config;

use std::env;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Context;
use directories::{BaseDirs, ProjectDirs};
use partitur_backend_acp::AcpBackend;
use partitur_core::{App, GlobalSettings};
use partitur_git::GitCli;
use partitur_mcp::ToolServer;
use partitur_store::{FileStore, StatePaths};
use tracing_subscriber::EnvFilter;

use crate::config::{Config, global_settings, resolve_backends};

fn main() -> anyhow::Result<()> {
    let started = Instant::now();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("partitur=info")),
        )
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .init();

    let dirs = ProjectDirs::from("io", "hertlein", "partitur")
        .context("there is no home folder to keep the configuration and state in")?;
    let config_file = dirs.config_dir().join("config.toml");
    tracing::info!(config = %config_file.display(), data = %dirs.data_dir().display(), "starting");
    let config = Config::load(&config_file)?;
    let global = GlobalSettings {
        layer: global_settings(&config, &config_file)?,
        home: BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()),
    };
    let store = FileStore::new(StatePaths::under(dirs.data_dir()));
    let resolved = resolve_backends(&config)?;
    let (tool_server, tool_calls) = ToolServer::start()?;
    let urls = tool_server.urls();
    let (backend, events) = AcpBackend::start(resolved.backends, move |session, orchestrator| {
        urls.session_url(session, orchestrator)
    })?;
    let (git, git_events) = GitCli::start();
    let mut app = App::load(
        Box::new(backend),
        Box::new(store),
        Box::new(git),
        resolved.infos,
        resolved.default,
        global,
    )?;
    if let Some(folder) = env::var_os("PARTITUR_OPEN").map(PathBuf::from) {
        app.open_project(&folder)
            .with_context(|| format!("failed to open {} as a project", folder.display()))?;
    }

    tracing::info!(
        startup_ms = started.elapsed().as_millis(),
        "opening the window"
    );
    // The app owns the backend. iced drops the app when the window closes, before `run`
    // returns, and dropping the backend shuts it down: every session's turn is cancelled and
    // every adapter process ends. Only then is the tool server dropped, which stops it, so no
    // session loses its tools while it still runs.
    partitur_iced::run(app, events, git_events, tool_calls)?;
    drop(tool_server);
    Ok(())
}
