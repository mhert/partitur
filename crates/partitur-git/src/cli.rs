//! The blocking runner that answers one query or runs one step with the `git` command line.
//!
//! Every git process runs as `git -C <folder> …` with no input and with
//! `GIT_TERMINAL_PROMPT=0`, so it can never wait for a password. A process that exits with a
//! non-zero status is reported as [`GitError::Failed`] with its subcommand and its trimmed
//! error output, and one that cannot be started as [`GitError::Spawn`]. A fetch, the only
//! command that talks to another machine, is stopped once it runs longer than the fetch limit.
//!
//! The runner blocks while git runs, so it is used only on the port's request threads.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use partitur_core::{GitAnswer, GitError, GitQuery, GitStep, Head, RepoInfo};

use crate::porcelain::{parse_branches, parse_remote_branches, parse_worktrees};

/// How often a running fetch is checked for having exited.
const POLL: Duration = Duration::from_millis(20);

/// The arguments of `worktree list` that list every worktree for [`parse_worktrees`].
const WORKTREES: [&str; 2] = ["--porcelain", "-z"];

/// The arguments of `for-each-ref` that list the local branches for [`parse_branches`].
const BRANCHES: [&str; 2] = ["--format=%(refname)%00%(upstream:remotename)", "refs/heads"];

/// The arguments of `for-each-ref` that list the remote branches for
/// [`parse_remote_branches`].
const REMOTE_BRANCHES: [&str; 2] = ["--format=%(refname)", "refs/remotes"];

/// Runs git commands for the port.
#[derive(Debug, Clone)]
pub(crate) struct Runner {
    /// The git program.
    pub(crate) program: String,
    /// Variables added to every git process's environment.
    pub(crate) env: Vec<(String, String)>,
    /// How long a fetch may run before it is stopped.
    pub(crate) fetch_limit: Duration,
}

impl Runner {
    /// Answers `query`.
    pub(crate) fn query(&self, query: &GitQuery) -> Result<GitAnswer, GitError> {
        match query {
            GitQuery::Inspect(repository) => self.inspect(repository).map(GitAnswer::Repo),
            GitQuery::Changes(worktree) => {
                let status = self.text(
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
                let args = ["--is-ancestor", branch, base].map(OsStr::new);
                let output = self.output(repository, "merge-base", &args)?;
                match output.status.code() {
                    Some(0) => Ok(GitAnswer::Merged(true)),
                    Some(1) => Ok(GitAnswer::Merged(false)),
                    _ => Err(failed("merge-base", &output)),
                }
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
            GitStep::RestoreHead { repository, head } => {
                let args = match head {
                    Head::Branch(name) => vec![OsStr::new(name)],
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
        let worktrees = self.text(repository, "worktree list", &WORKTREES.map(OsStr::new))?;
        let branches = self.text(repository, "for-each-ref", &BRANCHES.map(OsStr::new))?;
        let remotes = self.text(repository, "for-each-ref", &REMOTE_BRANCHES.map(OsStr::new))?;
        Ok(RepoInfo {
            worktrees: parse_worktrees(&worktrees),
            branches: parse_branches(&branches),
            remote_branches: parse_remote_branches(&remotes),
        })
    }

    /// Fetches `remote` into `repository`, stopping git once it runs past the fetch limit.
    fn fetch(&self, repository: &Path, remote: &str) -> Result<(), GitError> {
        let mut child = self
            .command(repository, "fetch", &["--quiet", remote].map(OsStr::new))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| GitError::Spawn(error.to_string()))?;
        let started = Instant::now();
        loop {
            if child
                .try_wait()
                .map_err(|error| GitError::Spawn(error.to_string()))?
                .is_some()
            {
                let output = child
                    .wait_with_output()
                    .map_err(|error| GitError::Spawn(error.to_string()))?;
                return checked("fetch", output).map(drop);
            }
            let left = self.fetch_limit.saturating_sub(started.elapsed());
            if left.is_zero() {
                // The process may already have exited; there is nothing left to stop then.
                let _ = child.kill();
                let _ = child.wait();
                return Err(GitError::TimedOut {
                    command: "fetch".into(),
                    seconds: self.fetch_limit.as_secs(),
                });
            }
            std::thread::sleep(POLL.min(left));
        }
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
        git.arg("-C")
            .arg(dir)
            .args(command.split(' '))
            .args(args)
            .stdin(Stdio::null())
            .envs(self.env.iter().map(|(key, value)| (key, value)))
            .env("GIT_TERMINAL_PROMPT", "0");
        git
    }
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
    use std::path::PathBuf;

    use super::*;

    fn missing_program() -> Runner {
        Runner {
            program: "partitur-no-such-git".into(),
            env: Vec::new(),
            fetch_limit: Duration::from_secs(1),
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
}
