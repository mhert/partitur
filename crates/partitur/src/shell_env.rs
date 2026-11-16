//! The environment of the user's login shell, read once at the start.
//!
//! An application started from the macOS Finder inherits a minimal environment whose `PATH`
//! lacks the folders where Homebrew, npm and the user install their tools. Like other editors,
//! Partitur then asks the user's login shell for its whole environment, once and in the
//! background, so the window never waits on a slow `.zshrc`. The shell runs as a login and
//! interactive shell and is told to run this binary with the internal `--print-env`
//! argument, which prints every variable as `NAME=value` followed by a NUL byte. That output is
//! framed by a start and an end marker, so banners and notices printed by the startup files are
//! ignored, and the lookup ends as soon as the end marker is read, even when a background
//! process started by those files keeps the shell's output open. The names `PWD`, `OLDPWD`,
//! `SHLVL`, `_` and `PARTITUR_EXE` describe the shell itself and are dropped.
//!
//! When the shell cannot be started, answers without the markers or takes longer than
//! [`LIMIT`], it is stopped and the environment becomes just a `PATH`: the inherited one, or
//! the system folders when there is none, followed by `/opt/homebrew/bin`, `/usr/local/bin`
//! and `~/.local/bin` where they exist and are missing. Without a known home folder,
//! `~/.local/bin` is left out.
//!
//! Stopping the shell at the limit kills the shell alone: processes its startup files started
//! are left running as orphans, and the thread reading the shell's output may stay parked until
//! the last of them has closed it.

use std::env;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use directories::BaseDirs;
use partitur_core::{ProcessEnv, no_window};

/// Printed by the shell right before the environment.
pub const START_MARKER: &str = "__PARTITUR_ENV_START__";

/// Printed by the shell right after the environment.
pub const END_MARKER: &str = "__PARTITUR_ENV_END__";

/// How long the login shell may take to print its environment.
pub const LIMIT: Duration = Duration::from_secs(5);

/// The command the login shell runs: the environment printed by this binary, between the
/// markers.
const SCRIPT: &str = "printf %s __PARTITUR_ENV_START__; \"$PARTITUR_EXE\" --print-env; printf %s __PARTITUR_ENV_END__";

/// The variables that describe the shell process rather than the user's environment.
const DROPPED: [&str; 5] = ["PWD", "OLDPWD", "SHLVL", "_", "PARTITUR_EXE"];

/// The `PATH` taken when there is none at all.
const SYSTEM_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// Why the login shell gave no environment.
#[derive(Debug, thiserror::Error)]
pub enum ShellEnvError {
    /// The shell could not be started, or this binary's own path is unknown.
    #[error("the login shell could not be started: {0}")]
    Spawn(io::Error),
    /// The shell did not print the end marker within the limit.
    #[error("the login shell did not answer in time")]
    Timeout,
    /// The shell's output holds no start marker followed by an end marker.
    #[error("the login shell printed no environment")]
    NoMarkers,
}

/// Whether to ask the login shell: only on macOS, and only when standard input is not a
/// terminal, which a start from a terminal already has the shell's environment for.
#[must_use]
pub fn wanted(macos: bool, stdin_is_terminal: bool) -> bool {
    macos && !stdin_is_terminal
}

/// Writes every pair as `NAME=value` followed by a NUL byte, the format [`parse`] reads.
///
/// # Errors
///
/// Returns the error of writing to `out`.
pub fn print_env(
    out: &mut impl Write,
    vars: impl IntoIterator<Item = (String, String)>,
) -> io::Result<()> {
    for (name, value) in vars {
        write!(out, "{name}={value}\0")?;
    }
    out.flush()
}

/// The bytes between the first start marker and the end marker after it.
fn between(output: &[u8]) -> Option<&[u8]> {
    let start = find(output, START_MARKER.as_bytes())? + START_MARKER.len();
    let rest = output.get(start..)?;
    rest.get(..find(rest, END_MARKER.as_bytes())?)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The variables printed between the markers in `output`, without the dropped names. Empty
/// entries, entries that are not UTF-8 and entries without `=` are skipped.
///
/// # Errors
///
/// Returns [`ShellEnvError::NoMarkers`] when `output` holds no start marker followed by an end
/// marker.
pub fn parse(output: &[u8]) -> Result<Vec<(String, String)>, ShellEnvError> {
    let framed = between(output).ok_or(ShellEnvError::NoMarkers)?;
    Ok(framed
        .split(|byte| *byte == 0)
        .filter_map(|entry| std::str::from_utf8(entry).ok())
        .filter_map(|entry| entry.split_once('='))
        .filter(|(name, _)| !DROPPED.contains(name))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect())
}

/// The environment used without the shell's: `path`, or the system folders when there is
/// none, followed by each fallback folder (`~` being `home`) that `exists` and is not on it.
/// Without a `home`, the folder under it is left out.
#[must_use]
pub fn fallback(
    path: Option<&str>,
    home: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> Vec<(String, String)> {
    let mut joined = path.unwrap_or(SYSTEM_PATH).to_owned();
    let folders = [
        Some(PathBuf::from("/opt/homebrew/bin")),
        Some(PathBuf::from("/usr/local/bin")),
        home.map(|home| home.join(".local/bin")),
    ];
    for folder in folders.into_iter().flatten() {
        let entry = folder.display().to_string();
        if exists(&folder) && !joined.split(':').any(|present| present == entry) {
            if !joined.is_empty() {
                joined.push(':');
            }
            joined.push_str(&entry);
        }
    }
    vec![("PATH".to_owned(), joined)]
}

/// Asks `shell` for its environment by having it run `exe --print-env` as a login and
/// interactive shell, waiting at most `limit` from the start.
///
/// # Errors
///
/// Returns [`ShellEnvError::Spawn`] when the shell cannot be started,
/// [`ShellEnvError::Timeout`] when it has not printed the end marker by the limit, and
/// [`ShellEnvError::NoMarkers`] when it closed its output without printing both markers.
pub fn from_shell(
    shell: &Path,
    exe: &Path,
    limit: Duration,
) -> Result<Vec<(String, String)>, ShellEnvError> {
    let deadline = Instant::now() + limit;
    let mut child = no_window(&mut Command::new(shell))
        .args(["-l", "-i", "-c", SCRIPT])
        .env("PARTITUR_EXE", exe)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(ShellEnvError::Spawn)?;
    let (chunks, received) = mpsc::channel();
    let stdout = child.stdout.take();
    // The reader may outlive the lookup: a background process the startup files started can
    // keep the output open long after the end marker.
    thread::spawn(move || {
        let Some(mut stdout) = stdout else { return };
        let mut chunk = [0; 4096];
        // Reads until the output closes or the lookup has stopped listening.
        while let Ok(read @ 1..) = stdout.read(&mut chunk)
            && chunks
                .send(chunk.get(..read).unwrap_or_default().to_vec())
                .is_ok()
        {}
    });
    let mut output = Vec::new();
    let result = loop {
        if between(&output).is_some() {
            break parse(&output);
        }
        // Checked on every round, because `recv_timeout` hands out queued output even at a
        // zero timeout: a background process writing without pause would never let it time out.
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break Err(ShellEnvError::Timeout);
        }
        match received.recv_timeout(left) {
            Ok(chunk) => output.extend_from_slice(&chunk),
            Err(RecvTimeoutError::Timeout) => break Err(ShellEnvError::Timeout),
            Err(RecvTimeoutError::Disconnected) => break parse(&output),
        }
    };
    // The shell has printed all it is asked for, or has run out of time: it is stopped either
    // way and reaped, so it leaves no zombie. Its exit status does not matter.
    let _ = child.kill();
    let _ = child.wait();
    result
}

/// The environment `shell` gives when asked to run `exe`, or the [`fallback`] on `path` and
/// `home` when it gives none. This is the body of [`start_with`]'s thread.
fn lookup(
    shell: &Path,
    exe: io::Result<PathBuf>,
    path: Option<&str>,
    home: Option<&Path>,
    limit: Duration,
) -> Vec<(String, String)> {
    let started = Instant::now();
    match exe
        .map_err(ShellEnvError::Spawn)
        .and_then(|exe| from_shell(shell, &exe, limit))
    {
        Ok(vars) => {
            let ms = started.elapsed().as_millis();
            let count = vars.len();
            tracing::info!(vars = count, ms, "process environment from the login shell");
            vars
        }
        Err(e) => {
            let ms = started.elapsed().as_millis();
            tracing::info!(reason = %e, ms, "process environment from the fallback folders");
            fallback(path, home, Path::exists)
        }
    }
}

/// The environment for every child process. Unless `wanted`, that is the inherited one with
/// nothing added, known at once. Otherwise it is pending until a thread named `shell-env` has
/// asked the login shell named by `$SHELL` (else `/bin/zsh`), or has fallen back to the
/// folders.
#[must_use]
pub fn start(wanted: bool) -> ProcessEnv {
    start_with(wanted, login_shell(), LIMIT)
}

/// The login shell named by `$SHELL`, else `/bin/zsh`.
fn login_shell() -> PathBuf {
    env::var_os("SHELL").map_or_else(|| "/bin/zsh".into(), PathBuf::from)
}

/// [`start`] with the shell to ask and the time it may take.
fn start_with(wanted: bool, shell: PathBuf, limit: Duration) -> ProcessEnv {
    if !wanted {
        return ProcessEnv::ready(Vec::new());
    }
    let (process_env, setter) = ProcessEnv::pending();
    // Should the thread not start, the setter is dropped with it, which leaves the
    // environment empty rather than pending for ever.
    let _ = thread::Builder::new()
        .name("shell-env".to_owned())
        .spawn(move || {
            let path = env::var("PATH").ok();
            let home = BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
            setter.set(lookup(
                &shell,
                env::current_exe(),
                path.as_deref(),
                home.as_deref(),
                limit,
            ));
        });
    process_env
}

#[cfg(test)]
mod tests;
