//! The helpers for the child processes Partitur starts: their environment, their console
//! window, their process tree, and the paths they are given.
//!
//! [`ProcessEnv`] is the environment every child process gets, known at once or filled in later
//! by a background thread. [`no_window`] keeps a console program from opening a console window
//! on Windows. [`canonical`] resolves a path to the form `git` prints. A program Partitur starts
//! may start programs of its own, as a git fetch starts ssh. Killing only the program would
//! leave those running. [`own_group`] starts a program in a process group of its own, and
//! [`kill_tree`] ends that program together with every process it started. On Unix the group is
//! a process group, ended with `kill -KILL -- -<pid>`; on Windows the program gets a new process
//! group and its tree is ended with `taskkill /T /F`, which follows the parent links of the
//! processes instead. Both run a helper program rather than a system call, so neither needs
//! `unsafe` code.

mod canonical;
mod env;
mod no_window;

use std::process::{Child, Command, Stdio};

pub use canonical::canonical;
pub use env::{ProcessEnv, ProcessEnvSetter};
pub use no_window::no_window;

/// The process creation flag that starts a program in a process group of its own.
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// Makes `command` start in a process group of its own and returns it for chaining. On Windows
/// the program also starts without a console window, as with [`no_window`].
pub fn own_group(command: &mut Command) -> &mut Command {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0)
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Flags set here replace those of `no_window`, so they carry its flag along.
        command.creation_flags(no_window::CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP)
    }
}

/// Ends `child` and every process it started, then waits for `child`. `child` must have been
/// started through [`own_group`]. A process that has already exited leaves nothing to end, so
/// every failure is ignored.
pub fn kill_tree(child: &mut Child) {
    let pid = child.id();
    #[cfg(unix)]
    let mut killer = {
        let mut killer = Command::new("kill");
        killer.args(["-KILL", "--", &format!("-{pid}")]);
        killer
    };
    #[cfg(windows)]
    let mut killer = {
        let mut killer = Command::new("taskkill");
        no_window(&mut killer);
        killer.args(["/T", "/F", "/PID", &pid.to_string()]);
        killer
    };
    let _ = killer
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    #[cfg(unix)]
    use std::io::BufRead;
    use std::process::Command;
    #[cfg(unix)]
    use std::process::Stdio;
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    #[cfg(unix)]
    use super::kill_tree;
    use super::own_group;
    #[cfg(unix)]
    use crate::testing::alive;

    #[cfg(unix)]
    #[test]
    fn kill_tree_ends_the_children_of_a_program_in_its_own_group() {
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 600 & echo $!; wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        let mut child = own_group(&mut command).spawn().unwrap();
        let mut pid = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut pid)
            .unwrap();
        let pid = pid.trim().to_owned();
        assert!(alive(&pid), "the grandchild {pid} never ran");

        kill_tree(&mut child);

        assert!(
            child.try_wait().unwrap().is_some(),
            "the child was not reaped"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while alive(&pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let survived = alive(&pid);
        if survived {
            let _ = Command::new("kill").args(["-KILL", &pid]).status();
        }
        assert!(!survived, "the grandchild {pid} outlived kill_tree");
    }

    #[test]
    fn own_group_returns_the_same_command_and_keeps_its_program() {
        let mut command = Command::new("x");
        assert_eq!(own_group(&mut command).get_program(), "x");
    }
}
