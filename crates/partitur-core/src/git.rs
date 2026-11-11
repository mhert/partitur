//! The port Partitur reaches git through, and the vocabulary it speaks.
//!
//! [`Git`] is a synchronous command port, like [`Backend`](crate::Backend): each method queues
//! a request and returns at once. The outcome arrives later as one [`GitEvent`] that names the
//! same request, so the core never sees a future and never waits for a git process.
//!
//! A request either asks read-only [`GitQuery`]s, which all run and are answered one by one,
//! or runs [`GitStep`]s that change repositories, which run in order and stop at the first
//! failure. [`is_valid_branch_name`] checks a branch name before any step is queued; git checks
//! it again when the branch is created.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::ids::GitRequestId;

/// A command port to git, like [`Backend`](crate::Backend): every method queues a request and
/// returns at once; the outcome arrives later as one [`GitEvent`] naming the same `request`.
pub trait Git: Send {
    /// Queues read-only `queries`. All of them run; [`GitEvent::Answered`] carries one answer
    /// per query, in order.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::Unavailable`] when the request cannot be queued.
    fn query(&self, request: GitRequestId, queries: Vec<GitQuery>) -> Result<(), GitError>;

    /// Queues `steps` that change repositories. They run in order and stop at the first
    /// failure; [`GitEvent::Ran`] says how many succeeded.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::Unavailable`] when the request cannot be queued.
    fn run(&self, request: GitRequestId, steps: Vec<GitStep>) -> Result<(), GitError>;
}

/// A read-only question about a repository or worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitQuery {
    /// The worktrees, local branches and remote branches of the repository at this path,
    /// answered with [`GitAnswer::Repo`].
    Inspect(PathBuf),
    /// Uncommitted and untracked entries in a worktree, answered with [`GitAnswer::Changes`].
    Changes(PathBuf),
    /// Whether the local branch `branch` is merged into the local branch `base`, answered
    /// with [`GitAnswer::Merged`].
    IsMerged {
        /// The repository both branches live in.
        repository: PathBuf,
        /// The local branch that may be merged, by its name.
        branch: String,
        /// The local branch it may be merged into, by its name.
        base: String,
    },
}

/// The answer to one [`GitQuery`], one variant per query kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitAnswer {
    /// The answer to [`GitQuery::Inspect`].
    Repo(RepoInfo),
    /// The answer to [`GitQuery::Changes`]: how many entries are uncommitted or untracked.
    Changes(usize),
    /// The answer to [`GitQuery::IsMerged`].
    Merged(bool),
}

/// One change to a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitStep {
    /// Fetches `remote` into the repository.
    Fetch {
        /// The repository to fetch into.
        repository: PathBuf,
        /// The remote to fetch.
        remote: String,
    },
    /// `git switch --no-track -c <branch> <start>` in the main checkout.
    CreateBranch {
        /// The repository whose main checkout switches.
        repository: PathBuf,
        /// The new branch.
        branch: String,
        /// The commit or full ref it starts from, such as `refs/heads/main`, so a tag of
        /// the same name cannot stand in for a branch.
        start: String,
    },
    /// `git worktree add --no-track -b <branch> <path> <start>`.
    AddWorktree {
        /// The repository the worktree belongs to.
        repository: PathBuf,
        /// Where the new worktree goes.
        path: PathBuf,
        /// The new branch checked out in it.
        branch: String,
        /// The commit or full ref it starts from, such as `refs/heads/main`, so a tag of
        /// the same name cannot stand in for a branch.
        start: String,
    },
    /// Switches the main checkout back to `head`.
    RestoreHead {
        /// The repository whose main checkout switches.
        repository: PathBuf,
        /// The branch or commit to switch to.
        head: Head,
    },
    /// Removes a worktree, with `--force`.
    RemoveWorktree {
        /// The repository the worktree belongs to.
        repository: PathBuf,
        /// The worktree to remove.
        path: PathBuf,
    },
    /// Deletes a local branch, with `-D`.
    DeleteBranch {
        /// The repository the branch lives in.
        repository: PathBuf,
        /// The branch to delete.
        branch: String,
    },
}

impl GitStep {
    /// The repository this step changes.
    pub fn repository(&self) -> &Path {
        match self {
            Self::Fetch { repository, .. }
            | Self::CreateBranch { repository, .. }
            | Self::AddWorktree { repository, .. }
            | Self::RestoreHead { repository, .. }
            | Self::RemoveWorktree { repository, .. }
            | Self::DeleteBranch { repository, .. } => repository,
        }
    }
}

/// The outcome of one request queued through [`Git`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitEvent {
    /// The answers to a [`Git::query`] request, one per query, in order.
    Answered {
        /// The request being answered.
        request: GitRequestId,
        /// One answer or error per query.
        answers: Vec<Result<GitAnswer, GitError>>,
    },
    /// The outcome of a [`Git::run`] request: `done` steps succeeded; with `Err` the step
    /// after them failed and the rest did not run.
    Ran {
        /// The request being answered.
        request: GitRequestId,
        /// How many steps succeeded.
        done: usize,
        /// Whether every step succeeded, or why the next one failed.
        result: Result<(), GitError>,
    },
}

impl GitEvent {
    /// The request this event answers.
    pub fn request(&self) -> GitRequestId {
        match self {
            Self::Answered { request, .. } | Self::Ran { request, .. } => *request,
        }
    }
}

/// Why a git request could not be queued, or a query or step failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum GitError {
    /// The git port is not running or cannot take requests.
    #[error("git is unavailable: {0}")]
    Unavailable(String),
    /// The git process could not be started.
    #[error("cannot run git: {0}")]
    Spawn(String),
    /// Git ran and reported a failure.
    #[error("git {command} failed: {stderr}")]
    Failed {
        /// The git subcommand that failed.
        command: String,
        /// What git wrote to its error output.
        stderr: String,
    },
    /// Git ran too long and was stopped.
    #[error("git {command} took longer than {seconds} s and was stopped")]
    TimedOut {
        /// The git subcommand that was stopped.
        command: String,
        /// How long it was allowed to run.
        seconds: u64,
    },
}

/// What [`GitQuery::Inspect`] learns about a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    /// Every worktree; the first one is the main checkout.
    pub worktrees: Vec<WorktreeInfo>,
    /// The local branches.
    pub branches: Vec<LocalBranch>,
    /// The remote-tracking branches, such as `origin/main`.
    pub remote_branches: Vec<String>,
}

/// One worktree of a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    /// Where the worktree is.
    pub path: PathBuf,
    /// What it has checked out.
    pub head: Head,
    /// Whether git considers it prunable, as when its folder is gone.
    pub prunable: bool,
}

/// What a worktree has checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// A local branch, by name.
    Branch(String),
    /// A detached commit.
    Detached(String),
}

/// One local branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBranch {
    /// The branch name.
    pub name: String,
    /// The remote its upstream lives on, if it has one.
    pub upstream_remote: Option<String>,
}

impl RepoInfo {
    /// What the main checkout has checked out.
    pub fn head(&self) -> Option<&Head> {
        self.worktrees.first().map(|worktree| &worktree.head)
    }

    /// The branch the main checkout has checked out, `None` when its head is detached.
    pub fn current_branch(&self) -> Option<&str> {
        match self.head() {
            Some(Head::Branch(name)) => Some(name),
            _ => None,
        }
    }

    /// The names of the local branches, sorted.
    pub fn branch_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .branches
            .iter()
            .map(|branch| branch.name.clone())
            .collect();
        names.sort();
        names
    }

    /// Whether a local branch named `name` exists.
    pub fn has_branch(&self, name: &str) -> bool {
        self.branches.iter().any(|branch| branch.name == name)
    }

    /// The remote to fetch before starting from `base`: the upstream remote of `base`, else
    /// `origin`, when `<remote>/<base>` is a remote branch.
    pub fn fetch_remote(&self, base: &str) -> Option<&str> {
        let remote = self
            .branches
            .iter()
            .find(|branch| branch.name == base)
            .and_then(|branch| branch.upstream_remote.as_deref())
            .unwrap_or("origin");
        let wanted = format!("{remote}/{base}");
        self.remote_branches.contains(&wanted).then_some(remote)
    }

    /// The worktree (other than a prunable one) that has `branch` checked out.
    pub fn checked_out_at(&self, branch: &str) -> Option<&Path> {
        self.worktrees
            .iter()
            .find(|worktree| {
                !worktree.prunable && matches!(&worktree.head, Head::Branch(name) if name == branch)
            })
            .map(|worktree| worktree.path.as_path())
    }
}

/// Whether git accepts `name` as a branch name, by the rules of
/// `git check-ref-format --branch`.
///
/// A valid name is not empty, does not start with `-`, and is neither `@` nor `HEAD`. It holds
/// no `..`, no `@{`, no space, control character or any of `\ ~ ^ : ? * [`, and does not end
/// with `.`. Split at `/`, every part is non-empty, does not start with `.` and does not end
/// with `.lock`, compared case-sensitively as git does.
///
/// # Examples
///
/// ```
/// use partitur_core::is_valid_branch_name;
///
/// assert!(is_valid_branch_name("feat/login-retry"));
/// assert!(!is_valid_branch_name("feat..login"));
/// ```
pub fn is_valid_branch_name(name: &str) -> bool {
    !name.starts_with('-')
        && name != "@"
        && name != "HEAD"
        && !name.ends_with('.')
        && !name.contains("..")
        && !name.contains("@{")
        && !name.contains(|c: char| c.is_ascii_control() || "\\ ~^:?*[".contains(c))
        && name.split('/').all(|part| {
            !part.is_empty() && !part.starts_with('.') && part.strip_suffix(".lock").is_none()
        })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::*;

    fn branch(name: &str, upstream: Option<&str>) -> LocalBranch {
        LocalBranch {
            name: name.into(),
            upstream_remote: upstream.map(Into::into),
        }
    }

    fn worktree(path: &str, branch: &str, prunable: bool) -> WorktreeInfo {
        WorktreeInfo {
            path: PathBuf::from(path),
            head: Head::Branch(branch.into()),
            prunable,
        }
    }

    #[test]
    fn branch_names_follow_gits_rules() {
        for ok in ["feat/login-retry", "fix_1.2", "ümlaut", "a/b/c", "v1@2"] {
            assert!(is_valid_branch_name(ok), "{ok}");
        }
        for bad in [
            "",
            "-x",
            "@",
            "HEAD",
            "a..b",
            "a@{b",
            "a//b",
            "a\\b",
            "a b",
            "a~b",
            "a^b",
            "a:b",
            "a?b",
            "a*b",
            "a[b",
            "a\u{7}b",
            "a\u{7f}b",
            "/a",
            "a/",
            "a.",
            ".a",
            "a/.b",
            "a.lock",
            "a/b.lock/c",
        ] {
            assert!(!is_valid_branch_name(bad), "{bad}");
        }
    }

    #[test]
    fn the_fetch_remote_is_the_upstream_else_origin_and_needs_a_remote_branch() {
        let info = RepoInfo {
            worktrees: vec![worktree("/r", "main", false)],
            branches: vec![
                branch("main", Some("up")),
                branch("dev", None),
                branch("topic", None),
            ],
            remote_branches: vec!["up/main".into(), "origin/dev".into()],
        };
        let origin_only = RepoInfo {
            remote_branches: vec!["origin/main".into()],
            ..info.clone()
        };

        assert_eq!(info.fetch_remote("main"), Some("up"));
        assert_eq!(info.fetch_remote("dev"), Some("origin"));
        assert_eq!(info.fetch_remote("topic"), None);
        assert_eq!(origin_only.fetch_remote("main"), None);
    }

    #[test]
    fn checked_out_at_skips_prunable_worktrees() {
        let info = RepoInfo {
            worktrees: vec![
                worktree("/r", "main", false),
                worktree("/gone", "x", true),
                worktree("/wt/y", "y", false),
            ],
            branches: vec![branch("main", None), branch("x", None), branch("y", None)],
            remote_branches: Vec::new(),
        };

        assert_eq!(info.checked_out_at("x"), None);
        assert_eq!(info.checked_out_at("y"), Some(Path::new("/wt/y")));
        assert_eq!(info.checked_out_at("main"), Some(Path::new("/r")));
        assert_eq!(info.checked_out_at("none"), None);
    }

    #[test]
    fn the_main_checkout_names_the_head_and_the_current_branch() {
        let mut info = RepoInfo {
            worktrees: vec![worktree("/r", "main", false), worktree("/wt", "x", false)],
            branches: vec![branch("main", None), branch("b", None), branch("a", None)],
            remote_branches: Vec::new(),
        };

        assert_eq!(info.head(), Some(&Head::Branch("main".into())));
        assert_eq!(info.current_branch(), Some("main"));
        assert_eq!(info.branch_names(), ["a", "b", "main"]);
        assert!(info.has_branch("b"));
        assert!(!info.has_branch("x"));

        info.worktrees[0].head = Head::Detached("abc123".into());
        assert_eq!(info.head(), Some(&Head::Detached("abc123".into())));
        assert_eq!(info.current_branch(), None);

        info.worktrees.clear();
        assert_eq!(info.head(), None);
        assert_eq!(info.current_branch(), None);
    }

    #[test]
    fn a_git_event_names_its_request() {
        let answered = GitEvent::Answered {
            request: GitRequestId(3),
            answers: vec![Ok(GitAnswer::Merged(true))],
        };
        let ran = GitEvent::Ran {
            request: GitRequestId(4),
            done: 0,
            result: Ok(()),
        };

        assert_eq!(answered.request(), GitRequestId(3));
        assert_eq!(ran.request(), GitRequestId(4));
    }

    #[test]
    fn a_step_names_its_repository() {
        let steps = [
            GitStep::Fetch {
                repository: "/1".into(),
                remote: "origin".into(),
            },
            GitStep::CreateBranch {
                repository: "/2".into(),
                branch: "b".into(),
                start: "main".into(),
            },
            GitStep::AddWorktree {
                repository: "/3".into(),
                path: "/wt".into(),
                branch: "b".into(),
                start: "main".into(),
            },
            GitStep::RestoreHead {
                repository: "/4".into(),
                head: Head::Branch("main".into()),
            },
            GitStep::RemoveWorktree {
                repository: "/5".into(),
                path: "/wt".into(),
            },
            GitStep::DeleteBranch {
                repository: "/6".into(),
                branch: "b".into(),
            },
        ];

        let repositories: Vec<&Path> = steps.iter().map(GitStep::repository).collect();

        assert_eq!(
            repositories,
            ["/1", "/2", "/3", "/4", "/5", "/6"].map(Path::new)
        );
    }

    #[test]
    fn git_errors_describe_what_failed() {
        assert_eq!(
            GitError::Unavailable("stopped".into()).to_string(),
            "git is unavailable: stopped"
        );
        assert_eq!(
            GitError::Spawn("not found".into()).to_string(),
            "cannot run git: not found"
        );
        assert_eq!(
            GitError::Failed {
                command: "fetch".into(),
                stderr: "no remote".into()
            }
            .to_string(),
            "git fetch failed: no remote"
        );
        assert_eq!(
            GitError::TimedOut {
                command: "fetch".into(),
                seconds: 30
            }
            .to_string(),
            "git fetch took longer than 30 s and was stopped"
        );
    }
}
