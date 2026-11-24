//! The Partitur desktop application: wires the store, the agent backend, the orchestrator tool
//! server and the GUI together.
//!
//! - The configuration is `config.toml` in the platform's config folder (on Linux
//!   `~/.config/partitur/config.toml`, on macOS
//!   `~/Library/Application Support/mhert.partitur/config.toml`, on Windows
//!   `%APPDATA%\mhert\partitur\config\config.toml`); the [`config`] module
//!   lists its keys. It names the agent backends and the default one new sessions run on.
//!   Without the file one backend, `claude`, runs the Claude Code adapter `claude-agent-acp`. A
//!   backend whose command is `claude-agent-acp` runs it through `npx` (pinned to the version
//!   Partitur was tested with) when it is not installed. Any other configured command must be
//!   installed. Started on Linux, or on macOS from a terminal, a missing one stops the start
//!   with an error naming its backend; on Windows, and on macOS from the Finder (see below),
//!   it fails the opening of each session on its backend instead.
//! - Projects, sessions and transcripts are kept in the platform's data folder (on Linux
//!   `~/.local/share/partitur`, on macOS the same folder as the configuration, on Windows
//!   `%APPDATA%\mhert\partitur\data`). Its `agent-instructions` folder holds Partitur's tool
//!   instructions for agents, such as GitHub Copilot CLI, that read them from a folder named
//!   in `COPILOT_CUSTOM_INSTRUCTIONS_DIRS`; the backend writes it once the backends are known.
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
//!   The release workflow smokes every Linux package with `scripts/smoke.sh` and the macOS
//!   `Partitur.app` with `scripts/smoke-macos.sh`, which also requires that the environment
//!   came from the login shell. On Windows the release workflow installs the setup program,
//!   smokes the installed `partitur.exe` with `scripts/smoke-windows.ps1` and uninstalls it
//!   again. Any other argument is a usage error with exit status 2.
//! - MCP App widgets are rendered by CEF ([`partitur_cef`]), which starts on a thread of its own
//!   while the configuration is read and the backends start, and is waited for just before the
//!   window opens. When it does not start, the reason is logged and every widget falls back to
//!   its tool call's text; under `--smoke` that stops the start instead, except on macOS, where
//!   CEF widgets are not supported yet. CEF runs this executable again as its helper processes
//!   and its sandbox probe, which hand over to CEF before anything else, command line included.
//!   On Linux the binary finds libcef beside itself or in `../lib/partitur`.
//! - `RUST_LOG` filters the log written to stderr; the default is `partitur=info`.
//! - `PARTITUR_OPEN=<folder>` opens that folder as a project at startup, as if it had been
//!   picked in the folder dialog.
//! - On Windows no console window opens, so `--version` and the errors written to stderr are
//!   only seen when the output is redirected. An error that stops the start, a usage error
//!   included, is therefore also shown in a message dialog before the process exits, except
//!   under `--smoke`, which must never wait for a click.
//! - On Windows each adapter runs under `partitur --job-run` ([`job_run`]), which ends its
//!   whole process tree when the session stops. The backend list keeps showing the configured
//!   command.

// No console window opens on Windows: the program is a GUI. `--version` and errors printed
// to stderr are therefore only seen when the output is redirected, and `report` also shows an
// error in a dialog there.
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

mod cli;
mod config;
mod folders;
mod job_run;
mod shell_env;
#[cfg(test)]
mod test_support;
mod widget_engine;

use std::env;
use std::io::{self, IsTerminal};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::Context;
use directories::BaseDirs;
use partitur_backend_acp::AcpBackend;
use partitur_cef::CefWidgetHost;
use partitur_core::widget_host::{NoWidgetHost, WidgetHost, WidgetHostEvent};
use partitur_core::{App, GlobalSettings};
use partitur_git::GitCli;
use partitur_mcp::ToolServer;
use partitur_store::{FileReader, FileStore, StatePaths};
use tracing_subscriber::EnvFilter;

use crate::cli::{
    Command, needs_widgets, project_to_open, run_mode, shows_a_failed_start, version_line,
    waits_for_process_env,
};
use crate::config::{
    Config, Launch, bundled_adapter, defers_resolution, global_settings, launcher, listing,
    resolution, resolve_at_start,
};
use crate::folders::Folders;
use crate::widget_engine::Starting;

/// The receiver of the widget engine's events.
type WidgetEvents = futures::channel::mpsc::UnboundedReceiver<WidgetHostEvent>;

fn main() -> ExitCode {
    // CEF starts its helper processes, and the sandbox probe, by running this executable
    // again; those hand over to CEF before anything else runs, configuration included.
    if let Some(code) = partitur_cef::subprocess() {
        return ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    let command = match cli::parse(env::args_os().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("partitur: {error}");
            show_error(&error.to_string());
            return ExitCode::from(2);
        }
    };
    if let Command::Version = &command {
        println!("{}", version_line());
        return ExitCode::SUCCESS;
    }
    if let Command::PrintEnv = &command {
        let vars = env::vars_os().filter_map(|(name, value)| {
            Some((name.into_string().ok()?, value.into_string().ok()?))
        });
        return match shell_env::print_env(&mut io::stdout().lock(), vars) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        };
    }
    if let Command::JobRun { program, args } = &command {
        return ExitCode::from(u8::try_from(job_run::run(program, args)).unwrap_or(1));
    }
    match start(&command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:?}");
            if shows_a_failed_start(&command) {
                show_error(&format!("{error:#}"));
            }
            ExitCode::FAILURE
        }
    }
}

/// Shows `description` in an error dialog titled "Partitur" and waits until it is closed: on
/// Windows the program has no console, so stderr reaches nobody.
#[cfg(windows)]
fn show_error(description: &str) {
    let _ = rfd::MessageDialog::new()
        .set_title("Partitur")
        .set_level(rfd::MessageLevel::Error)
        .set_description(description)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

/// Does nothing: off Windows an error on stderr reaches the terminal or the session's log.
#[cfg(not(windows))]
fn show_error(_description: &str) {}

/// The widget host the thread `engine` started, and the receiver of its events.
///
/// When CEF did not start, or its thread failed, the reason is logged and no engine runs:
/// every tool call's widget falls back to its text. Under `--smoke` that is an error where
/// CEF is supported, so the release smoke proves the runtime loads.
fn widget_host(
    engine: Starting<(CefWidgetHost, WidgetEvents)>,
    command: &Command,
) -> anyhow::Result<(Box<dyn WidgetHost>, WidgetEvents)> {
    match engine.join() {
        Ok((host, events)) => {
            tracing::info!(sandboxed = host.sandboxed(), "CEF started");
            Ok((Box::new(host), events))
        }
        Err(error) if needs_widgets(command, cfg!(not(target_os = "macos"))) => {
            Err(error).context("the widget engine did not start")
        }
        Err(error) => {
            tracing::warn!(%error, "widgets fall back to text: the widget engine did not start");
            let (_engine, events) = futures::channel::mpsc::unbounded();
            Ok((Box::new(NoWidgetHost), events))
        }
    }
}

/// Starts the application for `command` and returns when its window has closed.
fn start(command: &Command) -> anyhow::Result<()> {
    let started = Instant::now();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("partitur=info")),
        )
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .init();
    // Started once the log is up, so a lookup that ends at once still logs its outcome, and
    // before the configuration is read, so the shell runs while the window is prepared.
    let shell_asked = shell_env::wanted(cfg!(target_os = "macos"), io::stdin().is_terminal());
    let process_env = shell_env::start(shell_asked);
    // CEF takes about a second to start on Linux (its sandbox probe runs first), so it starts
    // on a thread of its own while the configuration is read and the backends start.
    let widget_engine = Starting::spawn(partitur_cef::start);

    // Held until `start` returns, so a throwaway directory outlives the backend.
    let folders = Folders::open(matches!(command, Command::Smoke))?;
    let config_file = folders.config_file();
    tracing::info!(config = %config_file.display(), data = %folders.data_dir().display(), "starting");
    let config = Config::load(&config_file)?;
    let frame = config::window_frame(&config);
    let global = GlobalSettings {
        layer: global_settings(&config, &config_file)?,
        home: BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()),
    };
    let paths = StatePaths::under(folders.data_dir());
    let store = FileStore::new(paths.clone());
    let (tool_server, tool_calls) = ToolServer::start()?;
    let urls = tool_server.urls();
    let tool_url = move |session: &_, role| urls.session_url(session, role);
    // Without the shell, and off Windows, the backends resolve now, so a missing command
    // stops the start as before; when the shell is asked, and always on Windows, they resolve
    // on the runtime's thread once the environment is known, and a missing command then fails
    // only the sessions opened on its backend.
    let launch = Launch {
        // On Windows every adapter runs under this binary's `--job-run` launcher.
        launcher: launcher(cfg!(windows), env::current_exe().ok()),
        // Written by the runtime, off this thread, once the backends are known.
        instructions: Some(paths.instructions_dir()),
    };
    // A release ships the adapter beside the executable, which wins over `PATH` and `npx`.
    let bundle = env::current_exe()
        .ok()
        .and_then(|exe| bundled_adapter(&exe, cfg!(windows), cfg!(target_os = "macos")));
    let ((backend, events), infos, default) = if let Some(resolved) = resolve_at_start(
        &config,
        defers_resolution(shell_asked, cfg!(windows)),
        &process_env,
        bundle.as_ref(),
        &launch,
    )? {
        let started = AcpBackend::start(resolved.backends, tool_url, tool_server)?;
        (started, resolved.infos, resolved.default)
    } else {
        let (infos, default) = listing(&config)?;
        let pending = process_env.clone();
        let resolve = move || resolution(&config, &pending.wait(), bundle.as_ref(), &launch);
        (
            AcpBackend::start_deferred(resolve, tool_url, tool_server)?,
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
    app.set_reader(Box::new(FileReader::new(paths)));
    if let Some(folder) = project_to_open(command, env::var_os("PARTITUR_OPEN")) {
        app.open_project(&folder)
            .with_context(|| format!("failed to open {} as a project", folder.display()))?;
    }

    let (widget_host, widget_events) = widget_host(widget_engine, command)?;
    tracing::info!(
        startup_ms = started.elapsed().as_millis(),
        "opening the window"
    );
    // The app owns the backend, which owns the tool server. iced drops the app when the
    // window closes, before `run` returns, and dropping the backend shuts it down: every
    // session's turn is cancelled and every adapter process ends. Only then is the tool
    // server dropped, which stops it, so no session loses its tools while it still runs.
    partitur_iced::run(
        app,
        events,
        git_events,
        tool_calls,
        widget_host,
        widget_events,
        run_mode(command),
        process_env.clone(),
        frame,
    )?;
    // Outside the smoke check nothing waits for the shell lookup: the process exits with it
    // still running.
    if waits_for_process_env(command) {
        let _ = process_env.wait();
    }
    Ok(())
}
