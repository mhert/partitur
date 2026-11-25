//! The blocking runner that answers one query or runs one step with the `git` command line.
//!
//! Every git process runs as `git -C <folder> …` with no input and with
//! `GIT_TERMINAL_PROMPT=0`, so it can never wait for a password, and with
//! `GIT_OPTIONAL_LOCKS=0`, so a reading such as `status` never holds the index lock an agent's
//! own git command in the same folder may need. A process that exits with a
//! non-zero status is reported as [`GitError::Failed`] with its subcommand and its trimmed
//! error output, and one that cannot be started as [`GitError::Spawn`]. A fetch, the only
//! command that talks to another machine, is stopped once it runs longer than the fetch limit,
//! and every command a query runs once it runs longer than the read limit, so a git that hangs
//! on a dead mount or a lock never leaves a reading waiting. A step that changes a repository
//! other than by fetching runs to its end, since stopping it halfway could leave the repository
//! half changed. A stopped command is reported as [`GitError::TimedOut`]. Every git process
//! starts in a process group of its own, so stopping one also stops every process it started,
//! such as the ssh of a fetch.
//!
//! The variables that point git at a repository other than the one `-C` names, such as
//! `GIT_DIR` set by a git hook Partitur was started from, are removed from every process's
//! environment, so a command can never act on another repository than the one it names.
//!
//! The process environment the runner was given is added to every git process's inherited
//! environment, after waiting until it is known; the runner's own variables are added after it
//! and win.
//!
//! The runner blocks while git runs, so it is used only on the port's request threads.

use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use partitur_core::{
    GitAnswer, GitError, GitQuery, GitStep, Head, ProcessEnv, RepoInfo, no_window,
    process::{kill_tree, own_group},
};

use crate::porcelain::{parse_branches, parse_remote_branches, parse_worktrees};

/// How often a command with a limit is checked for having exited.
const POLL: Duration = Duration::from_millis(20);

/// The arguments of `worktree list` that list every worktree for [`parse_worktrees`].
const WORKTREES: [&str; 2] = ["--porcelain", "-z"];

/// The arguments of `for-each-ref` that list the local branches for [`parse_branches`].
const BRANCHES: [&str; 2] = ["--format=%(refname)%00%(upstream:remotename)", "refs/heads"];

/// The arguments of `for-each-ref` that list the remote branches for
/// [`parse_remote_branches`].
const REMOTE_BRANCHES: [&str; 2] = ["--format=%(refname)", "refs/remotes"];

/// The variables that would make git act on another repository, index or object store than
/// the one found from `-C`.
const REPOSITORY_ENV: [&str; 8] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
];

/// Runs git commands for the port.
#[derive(Debug, Clone)]
pub(crate) struct Runner {
    /// The git program.
    pub(crate) program: String,
    /// The process environment added to every git process's inherited one; a git process
    /// waits for it to be known before it starts.
    pub(crate) base: ProcessEnv,
    /// Variables added to every git process's environment.
    pub(crate) env: Vec<(String, String)>,
    /// How long a fetch may run before it is stopped.
    pub(crate) fetch_limit: Duration,
    /// How long one command of a query may run before it is stopped.
    pub(crate) read_limit: Duration,
}

impl Runner {
    /// Answers `query`.
    pub(crate) fn query(&self, query: &GitQuery) -> Result<GitAnswer, GitError> {
        match query {
            GitQuery::Inspect(repository) => self.inspect(repository).map(GitAnswer::Repo),
            GitQuery::Changes(worktree) => {
                let status = self.read_text(
                    worktree,
                    "status",
                    &["--porcelain=v1", "--untracked-files=all"].map(OsStr::new),
                )?;
                Ok(GitAnswer::Changes(
                    status.lines().filter(|line| !line.is_empty()).count(),
                ))
            }
            GitQuery::IsMerged {
                repository,
                branch,
                base,
            } => {
                // Full refs, so a tag named like either branch never answers for it.
                let (branch, base) = (format!("refs/heads/{branch}"), format!("refs/heads/{base}"));
                let args = ["--is-ancestor", &branch, &base].map(OsStr::new);
                let output = self.within(repository, "merge-base", &args, self.read_limit)?;
                match output.status.code() {
                    Some(0) => Ok(GitAnswer::Merged(true)),
                    Some(1) => Ok(GitAnswer::Merged(false)),
                    _ => Err(failed("merge-base", &output)),
                }
            }
            GitQuery::Toplevel(folder) => {
                let top = self.read_text(folder, "rev-parse", &[OsStr::new("--show-toplevel")])?;
                Ok(GitAnswer::Folder(top.trim_end_matches(['\n', '\r']).into()))
            }
        }
    }

    /// Runs `step`.
    pub(crate) fn step(&self, step: &GitStep) -> Result<(), GitError> {
        match step {
            GitStep::Fetch { repository, remote } => self.fetch(repository, remote),
            GitStep::CreateBranch {
                repository,
                branch,
                start,
            } => {
                let args = ["--no-track", "-c", branch, start].map(OsStr::new);
                self.text(repository, "switch", &args).map(drop)
            }
            GitStep::AddWorktree {
                repository,
                path,
                branch,
                start,
            } => {
                let args = [
                    OsStr::new("--no-track"),
                    OsStr::new("-b"),
                    OsStr::new(branch),
                    path.as_os_str(),
                    OsStr::new(start),
                ];
                self.text(repository, "worktree add", &args).map(drop)
            }
            GitStep::CheckoutWorktree {
                repository,
                path,
                branch,
            } => {
                let args = [path.as_os_str(), OsStr::new(branch)];
                self.text(repository, "worktree add", &args).map(drop)
            }
            GitStep::RestoreHead { repository, head } => {
                let args = match head {
                    // Without `--no-guess`, a missing branch with a remote namesake would be
                    // created, tracking the remote, instead of failing.
                    Head::Branch(name) => vec![OsStr::new("--no-guess"), OsStr::new(name)],
                    Head::Detached(commit) => vec![OsStr::new("--detach"), OsStr::new(commit)],
                };
                self.text(repository, "switch", &args).map(drop)
            }
            GitStep::RemoveWorktree { repository, path } => {
                let args = [OsStr::new("--force"), path.as_os_str()];
                self.text(repository, "worktree remove", &args).map(drop)
            }
            GitStep::DeleteBranch { repository, branch } => {
                let args = ["-D", branch].map(OsStr::new);
                self.text(repository, "branch", &args).map(drop)
            }
        }
    }

    fn inspect(&self, repository: &Path) -> Result<RepoInfo, GitError> {
        let worktrees = self.read_text(repository, "worktree list", &WORKTREES.map(OsStr::new))?;
        let branches = self.read_text(repository, "for-each-ref", &BRANCHES.map(OsStr::new))?;
        let remotes =
            self.read_text(repository, "for-each-ref", &REMOTE_BRANCHES.map(OsStr::new))?;
        Ok(RepoInfo {
            worktrees: parse_worktrees(&worktrees),
            branches: parse_branches(&branches),
            remote_branches: parse_remote_branches(&remotes),
        })
    }

    /// Fetches `remote` into `repository`, stopping git once it runs past the fetch limit.
    fn fetch(&self, repository: &Path, remote: &str) -> Result<(), GitError> {
        let args = ["--quiet", remote].map(OsStr::new);
        checked(
            "fetch",
            self.within(repository, "fetch", &args, self.fetch_limit)?,
        )
        .map(drop)
    }

    /// Runs `git -C <dir> <command> <args>` to its end, whatever its exit status, unless it
    /// runs longer than `limit`: then git is stopped and [`GitError::TimedOut`] returned. Its
    /// output is read while it runs, so a long one never stalls it.
    fn within(
        &self,
        dir: &Path,
        command: &str,
        args: &[&OsStr],
        limit: Duration,
    ) -> Result<Output, GitError> {
        let spawn_error = |error: std::io::Error| GitError::Spawn(error.to_string());
        let mut child = self
            .command(dir, command, args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(spawn_error)?;
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().map_err(spawn_error)? {
                break status;
            }
            let left = limit.saturating_sub(started.elapsed());
            if left.is_zero() {
                // Git's own children, such as the ssh of a fetch, are stopped with it.
                kill_tree(&mut child);
                return Err(GitError::TimedOut {
                    command: command.into(),
                    seconds: limit.as_secs(),
                });
            }
            std::thread::sleep(POLL.min(left));
        };
        Ok(Output {
            status,
            stdout: stdout.join().unwrap_or_default(),
            stderr: stderr.join().unwrap_or_default(),
        })
    }

    /// Runs `git -C <dir> <command> <args>` for a query and returns its output when it
    /// succeeds within the read limit.
    fn read_text(&self, dir: &Path, command: &str, args: &[&OsStr]) -> Result<String, GitError> {
        let output = checked(command, self.within(dir, command, args, self.read_limit)?)?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Runs `git -C <dir> <command> <args>` and returns its output when it succeeds.
    fn text(&self, dir: &Path, command: &str, args: &[&OsStr]) -> Result<String, GitError> {
        let output = checked(command, self.output(dir, command, args)?)?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Runs `git -C <dir> <command> <args>` to its end, whatever its exit status.
    fn output(&self, dir: &Path, command: &str, args: &[&OsStr]) -> Result<Output, GitError> {
        self.command(dir, command, args)
            .output()
            .map_err(|error| GitError::Spawn(error.to_string()))
    }

    /// The git process for `git -C <dir> <command> <args>`; `command` holds the subcommand's
    /// words separated by spaces.
    fn command(&self, dir: &Path, command: &str, args: &[&OsStr]) -> Command {
        let mut git = Command::new(&self.program);
        no_window(&mut git);
        own_group(&mut git);
        git.envs(self.base.wait().iter().map(|(key, value)| (key, value)));
        git.arg("-C")
            .arg(dir)
            .args(command.split(' '))
            .args(args)
            .stdin(Stdio::null())
            .envs(self.env.iter().map(|(key, value)| (key, value)))
            .env("GIT_TERMINAL_PROMPT", "0")
            // A reading such as `status` must not lock the index the agent's git may need.
            .env("GIT_OPTIONAL_LOCKS", "0");
        for key in REPOSITORY_ENV {
            git.env_remove(key);
        }
        git
    }
}

/// Reads `pipe` to its end on a thread of its own and hands back what it read.
fn drain(pipe: Option<impl Read + Send + 'static>) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            // What was read before a failure is all there is to report.
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    })
}

/// `output` when git succeeded, else why it failed.
fn checked(command: &str, output: Output) -> Result<Output, GitError> {
    if output.status.success() {
        Ok(output)
    } else {
        Err(failed(command, &output))
    }
}

fn failed(command: &str, output: &Output) -> GitError {
    GitError::Failed {
        command: command.to_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::path::PathBuf;

    #[cfg(unix)]
    use partitur_core::testing::alive;

    use super::*;

    fn missing_program() -> Runner {
        Runner {
            program: "partitur-no-such-git".into(),
            base: ProcessEnv::ready(Vec::new()),
            env: Vec::new(),
            fetch_limit: Duration::from_secs(1),
            read_limit: Duration::from_secs(1),
        }
    }

    #[test]
    fn a_git_that_cannot_be_started_is_a_spawn_error() {
        let runner = missing_program();
        let here = PathBuf::from(".");

        let answer = runner.query(&GitQuery::Changes(here.clone()));
        let fetched = runner.step(&GitStep::Fetch {
            repository: here,
            remote: "origin".into(),
        });

        for result in [answer.map(drop), fetched] {
            assert!(
                matches!(&result, Err(GitError::Spawn(reason)) if !reason.is_empty()),
                "expected a spawn error, got {result:?}"
            );
        }
    }

    #[test]
    fn the_process_env_reaches_git() {
        // A global config git cannot parse proves git saw the variable. An empty PATH would
        // not do: Windows still finds git through the parent's PATH and the system folders.
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("broken.gitconfig");
        std::fs::write(&config, "[broken\n").unwrap();
        let runner = Runner {
            program: "git".into(),
            base: ProcessEnv::ready(vec![(
                "GIT_CONFIG_GLOBAL".into(),
                config.to_string_lossy().into_owned(),
            )]),
            env: Vec::new(),
            fetch_limit: Duration::from_secs(1),
            read_limit: Duration::from_secs(1),
        };

        let result = runner
            .query(&GitQuery::Changes(PathBuf::from(".")))
            .map(drop);

        assert!(
            matches!(&result, Err(GitError::Failed { stderr, .. })
                if stderr.contains("bad config") && stderr.contains("broken.gitconfig")),
            "expected git to reject the config, got {result:?}"
        );
    }

    #[test]
    fn git_takes_no_optional_locks() {
        // A `git status` that refreshed the index would hold `index.lock` while the agent's
        // own git command in the same folder may need it.
        let git = missing_program().command(Path::new("."), "status", &[]);

        let locks = git
            .get_envs()
            .find(|(key, _)| *key == "GIT_OPTIONAL_LOCKS")
            .and_then(|(_, value)| value);

        assert_eq!(locks, Some(OsStr::new("0")));
    }

    /// A git program that hangs for five seconds whatever it is asked.
    #[cfg(unix)]
    fn hanging_program(dir: &Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        let program = dir.join("hanging-git");
        std::fs::write(&program, "#!/bin/sh\nsleep 5\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        program.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn a_hanging_query_is_stopped_at_the_read_limit() {
        let dir = tempfile::tempdir().unwrap();
        let runner = Runner {
            program: hanging_program(dir.path()),
            read_limit: Duration::from_millis(200),
            ..missing_program()
        };
        let started = Instant::now();

        let answer = runner.query(&GitQuery::Changes(PathBuf::from(".")));

        assert_eq!(
            answer,
            Err(GitError::TimedOut {
                command: "status".into(),
                seconds: 0
            })
        );
        // The program hangs for five seconds; anything well below that was stopped.
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(4),
            "stopped after {elapsed:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn timeout_ends_the_children_git_started() {
        use std::os::unix::fs::PermissionsExt;
        // A git that starts a child of its own, as a fetch starts ssh, and waits for it.
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("child.pid");
        let program = dir.path().join("forking-git");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\nsleep 600 &\necho $! > '{0}.tmp'\nmv '{0}.tmp' '{0}'\nwait\n",
                pid_file.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let runner = Runner {
            program: program.to_string_lossy().into_owned(),
            // Headroom: under load the shell may need a while to start the child and write its pid.
            read_limit: Duration::from_secs(3),
            ..missing_program()
        };

        let answer = runner.query(&GitQuery::Changes(PathBuf::from(".")));

        assert_eq!(
            answer,
            Err(GitError::TimedOut {
                command: "status".into(),
                seconds: 3
            })
        );
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        let pid = pid.trim();
        // A killed child is a zombie until init reaps it, which `kill -0` still counts as alive.
        let running = |pid: &str| {
            alive(pid)
                && Command::new("ps")
                    .args(["-o", "stat=", "-p", pid])
                    .output()
                    .is_ok_and(|out| !String::from_utf8_lossy(&out.stdout).trim().starts_with('Z'))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while running(pid) && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }
        let survived = running(pid);
        if survived {
            let _ = Command::new("kill").args(["-KILL", pid]).status();
        }
        assert!(!survived, "git's child {pid} outlived the timeout");
    }
}
