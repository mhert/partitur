//! The hidden `--job-run` mode of the binary: `partitur --job-run -- <program> [args…]` runs
//! `program` with `args` and exits with its exit code, 1 when it has none (it was ended by a
//! signal) and 127 when it cannot be started.
//!
//! It exists for Windows, where the ACP crate ends a stopped session by killing only the
//! adapter process it started, so node and everything it started would keep running. There
//! every adapter is started through this mode: it first puts itself into a job object that
//! kills every process in it once its last handle closes, so the program and all it starts
//! inherit the job, and killing the launcher ends the whole tree. When the job cannot be set
//! up it says so on stderr and runs the program anyway. The program inherits the launcher's
//! stdin, stdout and stderr, so the ACP pipes pass straight through, and starts without a
//! console window.
//!
//! Nothing off Windows starts this mode; there it only runs the program, which is how its
//! argument passing and exit codes are tested.

use std::ffi::{OsStr, OsString};
use std::process::Command;

use partitur_core::no_window;

/// Runs `program` with `args` and returns the exit code the launcher exits with: the
/// program's own, 1 when it has none and 127 when it cannot be started. On Windows the
/// launcher first joins a kill-on-close job, which it holds until this returns.
pub fn run(program: &OsStr, args: &[OsString]) -> i32 {
    #[cfg(windows)]
    let _job = contain()
        .inspect_err(|error| eprintln!("partitur: cannot contain the adapter: {error}"))
        .ok();
    match no_window(Command::new(program).args(args)).status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!(
                "partitur: cannot start {}: {error}",
                program.to_string_lossy()
            );
            127
        }
    }
}

/// A job that kills every process in it when its last handle closes, with this process
/// assigned to it, so every process started from here on is in it too.
///
/// # Errors
///
/// Returns the error of creating the job, setting its limit or joining it.
#[cfg(windows)]
fn contain() -> Result<win32job::Job, win32job::JobError> {
    let job = win32job::Job::create()?;
    let mut info = job.query_extended_limit_info()?;
    info.limit_kill_on_job_close();
    job.set_extended_limit_info(&info)?;
    job.assign_current_process()?;
    Ok(job)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn args(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[cfg(unix)]
    #[test]
    fn the_launcher_passes_the_exit_code_through() {
        assert_eq!(run(OsStr::new("sh"), &args(&["-c", "exit 3"])), 3);
        assert_eq!(run(OsStr::new("sh"), &args(&["-c", "exit 0"])), 0);
    }

    #[cfg(windows)]
    #[test]
    fn the_launcher_passes_the_exit_code_through() {
        assert_eq!(run(OsStr::new("cmd"), &args(&["/c", "exit 3"])), 3);
        assert_eq!(run(OsStr::new("cmd"), &args(&["/c", "exit 0"])), 0);
    }

    #[test]
    fn a_program_that_cannot_start_is_127() {
        assert_eq!(run(OsStr::new("/nonexistent/program"), &[]), 127);
    }

    #[cfg(unix)]
    #[test]
    fn a_child_killed_by_a_signal_is_1() {
        assert_eq!(run(OsStr::new("sh"), &args(&["-c", "kill -9 $$"])), 1);
    }
}
