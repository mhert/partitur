//! The Partitur desktop application: wires the store, the agent backend and the GUI together.
//!
//! - The configuration is `config.toml` in the platform's config folder (on Linux
//!   `~/.config/partitur/config.toml`); the [`config`] module lists its keys. Without the
//!   file the Claude Code adapter `claude-agent-acp` runs, through `npx` (pinned to the
//!   version Partitur was tested with) when it is not installed. Any other configured
//!   command must be installed; a missing one stops the start with an error naming it.
//! - Projects, sessions and transcripts are kept in the platform's data folder (on Linux
//!   `~/.local/share/partitur`).
//! - `RUST_LOG` filters the log written to stderr; the default is `partitur=info`.
//! - `PARTITUR_OPEN=<folder>` opens that folder as a project at startup, as if it had been
//!   picked in the folder dialog.

mod config;

use std::collections::HashMap;
use std::env;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Context;
use directories::ProjectDirs;
use partitur_backend_acp::AcpBackend;
use partitur_core::{App, BackendId, BackendInfo};
use partitur_store::{FileStore, StatePaths};
use tracing_subscriber::EnvFilter;

use crate::config::{Config, resolve_backend};

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
    let store = FileStore::new(StatePaths::under(dirs.data_dir()));
    // One backend runs every session until the configuration names several.
    let claude = BackendId::try_from("claude")?;
    let adapters = HashMap::from([(claude.clone(), resolve_backend(&config.backend)?)]);
    let (backend, events) = AcpBackend::start(adapters)?;
    let backends = vec![BackendInfo {
        id: claude.clone(),
        name: claude.to_string(),
        command: "claude-agent-acp".into(),
    }];
    let mut app = App::load(Box::new(backend), Box::new(store), backends, claude)?;
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
    // every adapter process ends.
    partitur_iced::run(app, events)?;
    Ok(())
}
