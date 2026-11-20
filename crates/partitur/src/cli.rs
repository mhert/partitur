//! The command line of the `partitur` binary: no argument starts the application, `--version`
//! prints the version and `--smoke` opens the window on throwaway state and exits after the
//! first frame. Besides the two internal modes below, anything else is a usage error.
//!
//! Only the smoke check waits for the process environment after its window has closed, so the
//! outcome of the login shell lookup is logged before it exits; a normal start exits at once.
//!
//! `--print-env` is internal and left out of the usage text: it prints this process's
//! environment for the login shell lookup in `shell_env`, which runs this binary through the
//! user's shell.
//!
//! `--job-run -- <program> [args…]` is internal too: it runs one agent adapter in
//! [`crate::job_run`]'s launcher, which on Windows ends the adapter's whole process tree when
//! the session stops. Everything after the `--` belongs to the program.

use std::ffi::OsString;
use std::path::PathBuf;

use partitur_iced::Run;

/// What the command line asks the binary to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Start the application and run it until its window is closed.
    Run,
    /// Print the version and exit.
    Version,
    /// Open the window on throwaway state and exit after its first frame.
    Smoke,
    /// Print the environment as NUL-terminated `NAME=value` pairs and exit; internal.
    PrintEnv,
    /// Run `program` with `args` in the launcher of [`crate::job_run`] and exit with its exit
    /// code; internal.
    JobRun {
        /// The program to run.
        program: OsString,
        /// Its arguments.
        args: Vec<OsString>,
    },
}

/// The command line holds an argument that fits no command.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown argument `{}`; usage: partitur [--version | --smoke]", .argument.to_string_lossy())]
pub struct UsageError {
    argument: OsString,
}

impl UsageError {
    /// An error naming `argument`, the first one that does not fit.
    #[must_use]
    pub fn new(argument: OsString) -> Self {
        Self { argument }
    }
}

/// The command `args` ask for; `args` leaves out the program's name.
///
/// # Errors
///
/// Returns a [`UsageError`] naming the first argument that is not the only one of `--version`,
/// `--smoke` or `--print-env`, which includes a second argument after any of them. After
/// `--job-run` it names the argument in place of `--`, or `--job-run` itself when the `--` or
/// the program after it is missing.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, UsageError> {
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Ok(Command::Run);
    };
    let command = if first == "--version" {
        Command::Version
    } else if first == "--smoke" {
        Command::Smoke
    } else if first == "--print-env" {
        Command::PrintEnv
    } else if first == "--job-run" {
        return job_run(args, first);
    } else {
        return Err(UsageError::new(first));
    };
    match args.next() {
        Some(extra) => Err(UsageError::new(extra)),
        None => Ok(command),
    }
}

/// The `--job-run` command of the arguments after the flag itself, `flag`.
fn job_run(
    mut args: impl Iterator<Item = OsString>,
    flag: OsString,
) -> Result<Command, UsageError> {
    match args.next() {
        Some(separator) if separator == "--" => {}
        Some(other) => return Err(UsageError::new(other)),
        None => return Err(UsageError::new(flag)),
    }
    let program = args.next().ok_or_else(|| UsageError::new(flag))?;
    Ok(Command::JobRun {
        program,
        args: args.collect(),
    })
}

/// The line `--version` prints: the program's name and its version.
#[must_use]
pub fn version_line() -> String {
    format!("partitur {}", env!("CARGO_PKG_VERSION"))
}

/// The folder to open as a project at the start: `PARTITUR_OPEN`'s value, except for the smoke
/// check, which opens none.
#[must_use]
pub fn project_to_open(command: &Command, partitur_open: Option<OsString>) -> Option<PathBuf> {
    match command {
        Command::Smoke => None,
        Command::Run | Command::Version | Command::PrintEnv | Command::JobRun { .. } => {
            partitur_open.map(PathBuf::from)
        }
    }
}

/// How long the window stays open for `command`: the smoke check ends it at the first frame.
#[must_use]
pub fn run_mode(command: &Command) -> Run {
    match command {
        Command::Smoke => Run::FirstFrame,
        Command::Run | Command::Version | Command::PrintEnv | Command::JobRun { .. } => {
            Run::UntilClosed
        }
    }
}

/// Whether `command` waits, once the window has closed, for the process environment to be
/// known: only the smoke check does, so the outcome of the shell lookup is in its log. Any
/// other start exits at once, however long the lookup still takes.
#[must_use]
pub fn waits_for_process_env(command: &Command) -> bool {
    match command {
        Command::Smoke => true,
        Command::Run | Command::Version | Command::PrintEnv | Command::JobRun { .. } => false,
    }
}

/// Whether an error that stops the start of `command` is also shown in a message dialog where
/// the platform has one for it: always but in the smoke check, which must never wait for a
/// click.
#[must_use]
pub fn shows_a_failed_start(command: &Command) -> bool {
    match command {
        Command::Smoke => false,
        Command::Run | Command::Version | Command::PrintEnv | Command::JobRun { .. } => true,
    }
}

/// Whether a widget engine that does not start stops the start of `command`: only in the smoke
/// check, which proves that the engine's runtime loads, and only where the platform supports
/// widgets at all (`supported`).
#[must_use]
pub fn needs_widgets(command: &Command, supported: bool) -> bool {
    match command {
        Command::Smoke => supported,
        Command::Run | Command::Version | Command::PrintEnv | Command::JobRun { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn args(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn no_argument_runs_the_application() {
        assert_eq!(parse(args(&[])), Ok(Command::Run));
    }

    #[test]
    fn the_two_flags_select_their_commands() {
        assert_eq!(parse(args(&["--version"])), Ok(Command::Version));
        assert_eq!(parse(args(&["--smoke"])), Ok(Command::Smoke));
    }

    #[test]
    fn the_internal_flag_prints_the_environment() {
        assert_eq!(parse(args(&["--print-env"])), Ok(Command::PrintEnv));
        assert_eq!(
            parse(args(&["--print-env", "x"])),
            Err(UsageError::new(OsString::from("x")))
        );
    }

    #[test]
    fn the_job_run_flag_takes_the_program_after_the_separator() {
        assert_eq!(
            parse(args(&["--job-run", "--", "npx.cmd", "--yes", "pkg"])),
            Ok(Command::JobRun {
                program: OsString::from("npx.cmd"),
                args: args(&["--yes", "pkg"]),
            })
        );
        assert_eq!(
            parse(args(&["--job-run", "--", "agent"])),
            Ok(Command::JobRun {
                program: OsString::from("agent"),
                args: Vec::new(),
            })
        );
    }

    #[test]
    fn the_job_run_flag_needs_a_separator_and_a_program() {
        assert_eq!(
            parse(args(&["--job-run"])),
            Err(UsageError::new(OsString::from("--job-run")))
        );
        assert_eq!(
            parse(args(&["--job-run", "x"])),
            Err(UsageError::new(OsString::from("x")))
        );
        assert_eq!(
            parse(args(&["--job-run", "--"])),
            Err(UsageError::new(OsString::from("--job-run")))
        );
    }

    #[test]
    fn anything_else_is_a_usage_error() {
        for bad in [
            &["--smoke", "--version"][..],
            &["--version", "--smoke"],
            &["-v"],
            &["--help"],
            &["some/path"],
            &["--version", "x"],
            &["--smoke", "x"],
        ] {
            assert!(parse(args(bad)).is_err(), "{bad:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_argument_that_is_not_utf8_is_a_usage_error() {
        use std::os::unix::ffi::OsStringExt;
        assert!(parse([OsString::from_vec(vec![0xff])]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn an_argument_that_is_not_unicode_is_a_usage_error() {
        use std::os::windows::ffi::OsStringExt;
        // A lone surrogate is valid UTF-16 storage but no Unicode text.
        assert!(parse([OsString::from_wide(&[0xD800])]).is_err());
    }

    #[test]
    fn the_error_names_the_first_argument_that_does_not_fit() {
        assert_eq!(
            UsageError::new(OsString::from("--help")).to_string(),
            "unknown argument `--help`; usage: partitur [--version | --smoke]"
        );
        assert_eq!(
            parse(args(&["--smoke", "--version"]))
                .unwrap_err()
                .to_string(),
            "unknown argument `--version`; usage: partitur [--version | --smoke]"
        );
        assert_eq!(
            parse(args(&["--help"])).unwrap_err().to_string(),
            "unknown argument `--help`; usage: partitur [--version | --smoke]"
        );
    }

    #[test]
    fn the_version_line_names_the_program_and_its_version() {
        assert_eq!(
            version_line(),
            format!("partitur {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn only_a_run_opens_the_project_named_in_the_environment() {
        assert_eq!(
            project_to_open(&Command::Run, Some("/p".into())),
            Some(PathBuf::from("/p"))
        );
        assert_eq!(project_to_open(&Command::Run, None), None);
        assert_eq!(project_to_open(&Command::Smoke, Some("/p".into())), None);
    }

    #[test]
    fn only_the_smoke_check_ends_at_the_first_frame() {
        assert_eq!(run_mode(&Command::Smoke), Run::FirstFrame);
        assert_eq!(run_mode(&Command::Run), Run::UntilClosed);
    }

    #[test]
    fn only_the_smoke_check_waits_for_the_process_environment() {
        assert!(waits_for_process_env(&Command::Smoke));
        assert!(!waits_for_process_env(&Command::Run));
    }

    #[test]
    fn only_the_smoke_check_needs_widgets_where_they_are_supported() {
        assert!(needs_widgets(&Command::Smoke, true));
        assert!(!needs_widgets(&Command::Smoke, false));
        assert!(!needs_widgets(&Command::Run, true));
    }

    #[test]
    fn only_the_smoke_check_never_shows_a_failed_start() {
        assert!(!shows_a_failed_start(&Command::Smoke));
        assert!(shows_a_failed_start(&Command::Run));
    }
}
