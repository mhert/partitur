//! The Partitur desktop application: wires the store, the agent backend, the orchestrator tool
//! server and the GUI together.
//!
//! - The configuration is `config.toml` in the platform's config folder (on Linux
//!   `~/.config/partitur/config.toml`, on macOS
//!   `~/Library/Application Support/mhert.partitur/config.toml`); the [`config`] module
//!   lists its keys. It names the agent backends and the default one new sessions run on.
//!   Without the file one backend, `claude`, runs the Claude Code adapter `claude-agent-acp`. A
//!   backend whose command is `claude-agent-acp` runs it through `npx` (pinned to the version
//!   Partitur was tested with) when it is not installed. Any other configured command must be
//!   installed; a missing one stops the start with an error naming its backend.
//! - Projects, sessions and transcripts are kept in the platform's data folder (on Linux
//!   `~/.local/share/partitur`, on macOS the same folder as the configuration).
//! - Started on macOS without a terminal, from the Finder or the Dock, Partitur reads the login
//!   shell's environment in the background ([`shell_env`]) while the window opens. Adapters,
//!   git and link openers wait for it on their own threads; the backends are resolved once it
//!   is known, so a configured command that is missing then fails the opening of each session
//!   on that backend with the "not on PATH" message instead of stopping the start. Closing the
//!   window never waits for the lookup.
//! - `--version` prints the version and exits. `--smoke` opens the window on throwaway
//!   configuration and data folders in a temporary directory, which it removes again, and
//!   exits with status 0 after the first frame; it never reads or writes the user's folders.
//!   It waits for the process environment before it exits, so the lookup's outcome is logged.
//!   Any other argument is a usage error with exit status 2.
//! - `RUST_LOG` filters the log written to stderr; the default is `partitur=info`.
//! - `PARTITUR_OPEN=<folder>` opens that folder as a project at startup, as if it had been
//!   picked in the folder dialog.

mod cli;
mod config;
mod folders;
mod shell_env;

use std::env;
use std::io::{self, IsTerminal};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::Context;
use directories::BaseDirs;
use partitur_backend_acp::AcpBackend;
use partitur_core::{App, GlobalSettings};
use partitur_git::GitCli;
use partitur_mcp::ToolServer;
use partitur_store::{FileStore, StatePaths};
use tracing_subscriber::EnvFilter;

use crate::cli::{Command, project_to_open, run_mode, version_line, waits_for_process_env};
use crate::config::{Config, global_settings, listing, resolution, resolve_backends};
use crate::folders::Folders;

fn main() -> ExitCode {
    let command = match cli::parse(env::args_os().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("partitur: {error}");
            return ExitCode::from(2);
        }
    };
    if let Command::Version = command {
        println!("{}", version_line());
        return ExitCode::SUCCESS;
    }
    if let Command::PrintEnv = command {
        let vars = env::vars_os().filter_map(|(name, value)| {
            Some((name.into_string().ok()?, value.into_string().ok()?))
        });
        return match shell_env::print_env(&mut io::stdout().lock(), vars) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        };
    }
    match start(command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:?}");
            ExitCode::FAILURE
        }
    }
}

/// Starts the application for `command` and returns when its window has closed.
fn start(command: Command) -> anyhow::Result<()> {
    let started = Instant::now();
    let process_env = shell_env::start(shell_env::wanted(
        cfg!(target_os = "macos"),
        io::stdin().is_terminal(),
    ));
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("partitur=info")),
        )
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .init();

    // Held until `start` returns, so a throwaway directory outlives the backend.
    let folders = Folders::for_command(command)?;
    let config_file = folders.config_file();
    tracing::info!(config = %config_file.display(), data = %folders.data_dir().display(), "starting");
    let config = Config::load(&config_file)?;
    let global = GlobalSettings {
        layer: global_settings(&config, &config_file)?,
        home: BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()),
    };
    let store = FileStore::new(StatePaths::under(folders.data_dir()));
    let (tool_server, tool_calls) = ToolServer::start()?;
    let urls = tool_server.urls();
    let tool_url = move |session: &_, role| urls.session_url(session, role);
    // A known environment resolves the backends now, so a missing command stops the start as
    // before; a pending one resolves them on the runtime's thread once the shell has answered,
    // and a missing command then fails only the sessions opened on its backend.
    let ((backend, events), infos, default) = if let Some(base) = process_env.get() {
        let resolved = resolve_backends(&config, &base)?;
        let started = AcpBackend::start(resolved.backends, tool_url)?;
        (started, resolved.infos, resolved.default)
    } else {
        let (infos, default) = listing(&config)?;
        let pending = process_env.clone();
        let resolve = move || resolution(&config, &pending.wait());
        (
            AcpBackend::start_deferred(resolve, tool_url)?,
            infos,
            default,
        )
    };
    let (git, git_events) = GitCli::start();
    let git = git.with_process_env(process_env.clone());
    let mut app = App::load(
        Box::new(backend),
        Box::new(store),
        Box::new(git),
        infos,
        default,
        global,
    )?;
    if let Some(folder) = project_to_open(command, env::var_os("PARTITUR_OPEN")) {
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
    partitur_iced::run(
        app,
        events,
        git_events,
        tool_calls,
        run_mode(command),
        process_env.clone(),
    )?;
    drop(tool_server);
    // Outside the smoke check nothing waits for the shell lookup: the process exits with it
    // still running.
    if waits_for_process_env(command) {
        let _ = process_env.wait();
    }
    Ok(())
}
