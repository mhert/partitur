//! The background reading of git behind the Env tab's Git card.
//!
//! While the UI says the Env tab is open, the current session is read when the tab opens and
//! again whenever a turn of that session ends. A reading is one read-only request: `Inspect`
//! of the session's repository and `Changes` in the session's folder. Only a live session
//! that runs in a repository is read; an archived one is not, since its worktree may be gone.
//!
//! The answer applies only while it is the latest reading of its session, so a later reading
//! supersedes an earlier one. It finds the worktree git lists at the session's folder, paths
//! compared in their canonical form, and records its head and the uncommitted count; a folder
//! git does not list, or a query that failed, is recorded with the reason. The first branch a
//! reading finds for a session in the repository as it was checked out is saved as the
//! session's `seen_branch`, the branch later readings are compared with.

use std::path::{Path, PathBuf};

use super::git_work::Work;
use super::{App, AppError};
use crate::canonical;
use crate::git::{GitAnswer, GitError, GitQuery, Head, RepoInfo};
use crate::git_card::LiveGit;
use crate::ids::{GitRequestId, SessionId};
use crate::session::{Scope, Workspace};

/// The latest reading of one session's repository: the request in flight, if any, and what
/// the card shows.
#[derive(Debug, Clone)]
pub(super) struct EnvGit {
    /// The reading in flight; its answer is the only one taken.
    request: Option<GitRequestId>,
    /// What the card shows.
    live: LiveGit,
}

impl App {
    /// Records whether the Env tab is showing; `true` queues a reading of the current
    /// session, as [`check_env_git`](Self::check_env_git) does.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Git`] when the reading cannot be queued.
    pub fn set_env_open(&mut self, open: bool) -> Result<(), AppError> {
        self.env_open = open;
        if open { self.check_env_git() } else { Ok(()) }
    }

    /// Queues a reading of the current session's repository when the session is live and
    /// runs in a repository; does nothing otherwise. The card shows it checking until the
    /// answer arrives.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Git`] when the request cannot be queued; the card is left as it
    /// was.
    pub fn check_env_git(&mut self) -> Result<(), AppError> {
        let Some(id) = self.current.clone() else {
            return Ok(());
        };
        let Ok(session) = self.session(&id) else {
            return Ok(());
        };
        let Scope::Repository(repository) = &session.scope else {
            return Ok(());
        };
        if session.archived.is_some() {
            return Ok(());
        }
        let folder = session.cwd.clone();
        let Some(path) = self.project(&session.project).ok().and_then(|project| {
            project
                .repositories
                .iter()
                .find(|candidate| &candidate.id == repository)
                .map(|found| found.path.clone())
        }) else {
            return Ok(());
        };
        let work = Work::EnvCheck {
            session: id.clone(),
            folder: folder.clone(),
        };
        let queries = vec![GitQuery::Inspect(path), GitQuery::Changes(folder)];
        let request = self.git_work.query(&*self.git, work, queries)?;
        self.env_git.insert(
            id,
            EnvGit {
                request: Some(request),
                live: LiveGit::Checking,
            },
        );
        Ok(())
    }

    /// Reads the current session again after a turn of `session` ended, when the Env tab is
    /// open on it.
    ///
    /// # Errors
    ///
    /// As [`check_env_git`](Self::check_env_git).
    pub(super) fn recheck_env_git(&mut self, session: &SessionId) -> Result<(), AppError> {
        if self.env_open && self.current.as_ref() == Some(session) {
            self.check_env_git()
        } else {
            Ok(())
        }
    }

    /// Takes the `answers` to the reading `request` of `session` in `folder`, when it is the
    /// session's latest reading.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Store`] when the first branch seen cannot be saved.
    pub(super) fn env_git_read(
        &mut self,
        session: &SessionId,
        folder: &Path,
        request: GitRequestId,
        answers: Vec<Result<GitAnswer, GitError>>,
    ) -> Result<(), AppError> {
        let Some(entry) = self.env_git.get_mut(session) else {
            return Ok(());
        };
        if entry.request != Some(request) {
            return Ok(());
        }
        entry.request = None;
        entry.live = reading(folder, answers);
        match &entry.live {
            LiveGit::Read {
                head: Head::Branch(name),
                ..
            } => {
                let name = name.clone();
                self.remember_branch(session, name)
            }
            _ => Ok(()),
        }
    }

    /// What the card of `session` shows of git: its latest reading, or nothing checked.
    pub(super) fn env_live(&self, session: &SessionId) -> LiveGit {
        self.env_git
            .get(session)
            .map_or(LiveGit::NotChecked, |entry| entry.live.clone())
    }

    /// Saves `name` as the first branch seen of `session`, when it works in the repository as
    /// it was checked out and has none yet.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Store`] when the sessions cannot be saved.
    fn remember_branch(&mut self, session: &SessionId, name: String) -> Result<(), AppError> {
        let Some(found) = self
            .sessions
            .iter_mut()
            .find(|candidate| &candidate.id == session)
        else {
            return Ok(());
        };
        if found.workspace != Workspace::CheckedOut || found.seen_branch.is_some() {
            return Ok(());
        }
        found.seen_branch = Some(name);
        let project = found.project.clone();
        self.save_sessions(&project)
    }
}

/// What the answers to `[Inspect, Changes]` of a session in `folder` tell the card.
fn reading(folder: &Path, answers: Vec<Result<GitAnswer, GitError>>) -> LiveGit {
    let mut answers = answers.into_iter();
    let info = match answers.next() {
        Some(Ok(GitAnswer::Repo(info))) => info,
        other => return failed(other),
    };
    let uncommitted = match answers.next() {
        Some(Ok(GitAnswer::Changes(count))) => count,
        other => return failed(other),
    };
    match head_at(&info, folder) {
        Some(head) => LiveGit::Read {
            head: head.clone(),
            uncommitted,
        },
        None => LiveGit::Failed(format!(
            "git does not list {} as a worktree",
            folder.display()
        )),
    }
}

/// A failed reading for an `answer` that is not the one asked for: the error's text, or a
/// note that git answered something else.
fn failed(answer: Option<Result<GitAnswer, GitError>>) -> LiveGit {
    match answer {
        Some(Err(error)) => LiveGit::Failed(error.to_string()),
        _ => LiveGit::Failed("git gave an unexpected answer".into()),
    }
}

/// The head of the worktree `info` lists at `folder`, a prunable one aside.
fn head_at<'a>(info: &'a RepoInfo, folder: &Path) -> Option<&'a Head> {
    let folder = comparable(folder);
    info.worktrees
        .iter()
        .find(|worktree| !worktree.prunable && comparable(&worktree.path) == folder)
        .map(|worktree| &worktree.head)
}

/// `path` in its canonical form when it exists, as it is otherwise.
fn comparable(path: &Path) -> PathBuf {
    canonical(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_of_another_kind_fails_the_reading() {
        assert_eq!(
            failed(Some(Ok(GitAnswer::Merged(true)))),
            LiveGit::Failed("git gave an unexpected answer".into())
        );
        assert_eq!(
            reading(Path::new("/p"), Vec::new()),
            LiveGit::Failed("git gave an unexpected answer".into())
        );
    }
}
