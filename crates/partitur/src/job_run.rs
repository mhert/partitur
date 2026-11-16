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
//! argument passing and exit codes are tested. The unit tests run the program without the job,
//! which would otherwise take in the test binary itself; the launcher as a whole is tested as
//! the built binary.

use std::ffi::{OsStr, OsString};
use std::process::Command;

use partitur_core::no_window;

/// Runs `program` with `args` and returns the exit code the launcher exits with: the
/// program's own, 1 when it has none and 127 when it cannot be started. On Windows the
/// launcher first joins a kill-on-close job, which it keeps until the process exits.
pub fn run(program: &OsStr, args: &[OsString]) -> i32 {
    #[cfg(windows)]
    match contain() {
        // Forgotten rather than dropped: this process is in the job, and closing the job's only
        // handle would kill it before it could exit with the program's code. The handle closes
        // when the process exits, and that ends every process still in the job.
        Ok(job) => std::mem::forget(job),
        Err(error) => eprintln!("partitur: cannot contain the adapter: {error}"),
    }
    run_program(program, args)
}

/// Runs `program` with `args`, its standard streams inherited and without a console window,
/// waits for it and returns its exit code, 1 when it has none and 127 when it cannot be
/// started, which is also reported on stderr.
fn run_program(program: &OsStr, args: &[OsString]) -> i32 {
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

    #[cfg(unix)]
    fn args(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[cfg(unix)]
    #[test]
    fn the_program_s_exit_code_is_passed_through() {
        assert_eq!(run_program(OsStr::new("sh"), &args(&["-c", "exit 3"])), 3);
        assert_eq!(run_program(OsStr::new("sh"), &args(&["-c", "exit 0"])), 0);
    }

    #[test]
    fn a_program_that_cannot_start_is_127() {
        assert_eq!(run_program(OsStr::new("/nonexistent/program"), &[]), 127);
    }

    #[cfg(unix)]
    #[test]
    fn a_child_killed_by_a_signal_is_1() {
        assert_eq!(
            run_program(OsStr::new("sh"), &args(&["-c", "kill -9 $$"])),
            1
        );
    }
}
