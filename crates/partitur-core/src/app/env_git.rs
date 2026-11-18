//! The background reading of git behind the Env tab's Git card.
//!
//! While the UI says the Env tab is open, the current session is read when the tab opens,
//! whenever a turn of that session ends, however it ended (a crashed agent's included), and
//! when it is restored. A reading is one read-only
//! request: `Inspect` of the session's repository, `Changes` in the session's folder and
//! `Toplevel` of that folder. Only a live session that runs in a repository is read; an
//! archived one is not, since its worktree may be gone. A reading the git port cannot queue
//! shows on the card as failed, with the reason, instead of being reported to the caller.
//!
//! The answer applies only while it is the latest reading of its session, so a later reading
//! supersedes an earlier one. It finds the worktree git lists at the session's folder, either
//! as the session records it or as git resolves it, and records its head and the uncommitted
//! count; a folder git does not list, or a query that failed, is recorded with the reason. The
//! folder as git resolves it, when it differs, is saved as the session's `resolved_cwd`:
//! another spelling of its folder for the files the card counts as outside it, also while it is
//! archived or after a restart, so no path is resolved on the caller's thread. The first branch a reading finds for a session in the repository as it was
//! checked out is saved as the session's `seen_branch`, the branch later readings are
//! compared with. A deleted session's reading is forgotten with it.

use std::path::{Path, PathBuf};

use super::git_work::Work;
use super::{App, AppError};
use crate::canonical;
use crate::git::{GitAnswer, GitError, GitQuery, Head, RepoInfo};
use crate::git_card::LiveGit;
use crate::ids::{GitRequestId, ProjectId, SessionId};
use crate::session::{Scope, Session, Workspace};

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
    pub fn set_env_open(&mut self, open: bool) {
        self.env_open = open;
        if open {
            self.check_env_git();
        }
    }

    /// Queues a reading of the current session's repository when the session is live and
    /// runs in a repository; does nothing otherwise. The card shows it checking until the
    /// answer arrives, or failed with the reason when the git port cannot queue it.
    pub fn check_env_git(&mut self) {
        let Some((id, path, folder)) = self.env_target() else {
            return;
        };
        let work = Work::EnvCheck {
            session: id.clone(),
            folder: folder.clone(),
        };
        let queries = vec![
            GitQuery::Inspect(path),
            GitQuery::Changes(folder.clone()),
            GitQuery::Toplevel(folder),
        ];
        let (request, live) = match self.git_work.query(&*self.git, work, queries) {
            Ok(request) => (Some(request), LiveGit::Checking),
            Err(error) => (None, LiveGit::Failed(error.to_string())),
        };
        self.env_git.insert(id, EnvGit { request, live });
    }

    /// The current session, its repository's path and its folder, when it is live and runs in
    /// a repository the project still has.
    fn env_target(&self) -> Option<(SessionId, PathBuf, PathBuf)> {
        let session = self.session(self.current.as_ref()?).ok()?;
        let Scope::Repository(repository) = &session.scope else {
            return None;
        };
        if session.archived.is_some() {
            return None;
        }
        let project = self.project(&session.project).ok()?;
        let found = project
            .repositories
            .iter()
            .find(|candidate| &candidate.id == repository)?;
        Some((session.id.clone(), found.path.clone(), session.cwd.clone()))
    }

    /// Reads the current session again when the Env tab is open on it, as after a turn of
    /// `session` ended, however it ended.
    pub(super) fn recheck_env_git(&mut self, session: &SessionId) {
        if self.env_open && self.current.as_ref() == Some(session) {
            self.check_env_git();
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
        let (live, resolved) = reading(folder, answers);
        entry.live = live;
        let branch = match &entry.live {
            LiveGit::Read {
                head: Head::Branch(name),
                ..
            } => Some(name.clone()),
            _ => None,
        };
        if let Some(top) = resolved {
            self.keep_folder(session, top)?;
        }
        match branch {
            Some(name) => self.remember_branch(session, name),
            None => Ok(()),
        }
    }

    /// What the card of `session` shows of git: its latest reading, or nothing checked.
    pub(super) fn env_live(&self, session: &SessionId) -> LiveGit {
        self.env_git
            .get(session)
            .map_or(LiveGit::NotChecked, |entry| entry.live.clone())
    }

    /// Forgets the reading of `session`, as when it was deleted.
    pub(super) fn forget_env_git(&mut self, session: &SessionId) {
        self.env_git.remove(session);
    }

    /// Saves `top` as the folder git resolved the folder of `session` to, when it differs from
    /// the one saved.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Store`] when the sessions cannot be saved.
    pub(super) fn keep_folder(&mut self, id: &SessionId, top: PathBuf) -> Result<(), AppError> {
        let found = self.sessions.iter_mut().find(|session| &session.id == id);
        let changed = found.and_then(|session| resolve_into(session, top));
        changed.map_or(Ok(()), |project| self.save_sessions(&project))
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

/// What the answers to `[Inspect, Changes, Toplevel]` of a session in `folder` tell the
/// card, and the folder as git resolved it when the worktree was found.
fn reading(folder: &Path, answers: Vec<Result<GitAnswer, GitError>>) -> (LiveGit, Option<PathBuf>) {
    let mut answers = answers.into_iter();
    let info = match answers.next() {
        Some(Ok(GitAnswer::Repo(info))) => info,
        other => return (failed(other), None),
    };
    let uncommitted = match answers.next() {
        Some(Ok(GitAnswer::Changes(count))) => count,
        other => return (failed(other), None),
    };
    let top = match answers.next() {
        Some(Ok(GitAnswer::Folder(top))) => top,
        other => return (failed(other), None),
    };
    match head_at(&info, folder, &top) {
        Some(head) => (
            LiveGit::Read {
                head: head.clone(),
                uncommitted,
            },
            Some(top),
        ),
        None => (
            LiveGit::Failed(format!(
                "git does not list {} as a worktree",
                folder.display()
            )),
            None,
        ),
    }
}

/// Records `top` as the folder `session` resolves to, unless it is the session's own; the
/// project to save when that changed what was recorded.
fn resolve_into(session: &mut Session, top: PathBuf) -> Option<ProjectId> {
    let resolved = (top != session.cwd).then_some(top);
    let changed = session.resolved_cwd != resolved;
    session.resolved_cwd = resolved;
    changed.then(|| session.project.clone())
}

/// `path` as it is named and, when it resolves, as it resolves.
pub(super) fn both_spellings(path: PathBuf) -> Vec<PathBuf> {
    let resolved = canonical(&path).ok();
    std::iter::once(path).chain(resolved).collect()
}

/// A failed reading for an `answer` that is not the one asked for: the error's text, or a
/// note that git answered something else.
fn failed(answer: Option<Result<GitAnswer, GitError>>) -> LiveGit {
    match answer {
        Some(Err(error)) => LiveGit::Failed(error.to_string()),
        _ => LiveGit::Failed("git gave an unexpected answer".into()),
    }
}

/// The head of the worktree `info` lists at `folder`, as the session records it or as git
/// resolved it to `top`, a prunable one aside.
fn head_at<'a>(info: &'a RepoInfo, folder: &Path, top: &Path) -> Option<&'a Head> {
    info.worktrees
        .iter()
        .find(|worktree| !worktree.prunable && (worktree.path == top || worktree.path == folder))
        .map(|worktree| &worktree.head)
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
        let unexpected = LiveGit::Failed("git gave an unexpected answer".into());
        assert_eq!(
            reading(Path::new("/p"), Vec::new()),
            (unexpected.clone(), None)
        );
        let repo = Ok(GitAnswer::Repo(RepoInfo {
            worktrees: Vec::new(),
            branches: Vec::new(),
            remote_branches: Vec::new(),
        }));
        assert_eq!(
            reading(Path::new("/p"), vec![repo, Ok(GitAnswer::Changes(0))]),
            (unexpected, None)
        );
    }
}
