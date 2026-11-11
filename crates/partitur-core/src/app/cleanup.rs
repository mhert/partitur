//! What deleting a session offers to remove from git, and the removal after the delete.
//!
//! The delete sheet of a top-level session has one line per session of its family that made
//! a branch, in Worktree or New branch mode. Its lines are read in the background with one
//! read-only git request: whether the worktree is still there and how many uncommitted files
//! it has, and whether the branch still exists, is merged into the base it started from and
//! is checked out somewhere. Only the answer to the latest reading of a sheet fills it.
//!
//! Deleting removes only what the user checked on a line git has answered and whose
//! repository is not busy. Each repository is read again right before its removal, which
//! keeps it busy until the removal has run, and each checked worktree or branch is removed
//! only while it still is what the sheet showed. The main checkout is never removed, a
//! branch checked out anywhere except in the worktree removed just before it is never
//! deleted, and a Worktree session's branch goes only with its worktree. A branch still held
//! by a worktree whose folder is gone, one git considers prunable, is offered but cannot be
//! checked, since git refuses to delete it until that worktree is pruned. Every checked step
//! of one repository runs in one request, in order, by force. What is checked on a line
//! whose repository turned busy meanwhile is kept, and reported.

use std::fs;
use std::path::{Path, PathBuf};

use super::git_work::Work;
use super::{App, AppError};
use crate::git::{GitAnswer, GitError, GitQuery, GitStep, Head, RepoInfo, WorktreeInfo};
use crate::ids::{GitRequestId, ProjectId, SessionId};
use crate::project::{Project, Repository};
use crate::session::{Scope, Session, Workspace};
use crate::view_model::{BranchCleanup, CleanupView, WorktreeCleanup};

/// What the user checked on one line of the delete sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cleanup {
    /// The session the line is about.
    pub session: SessionId,
    /// Whether its worktree is removed.
    pub remove_worktree: bool,
    /// Whether its branch is deleted.
    pub delete_branch: bool,
}

/// The cleanup lines of one delete sheet.
#[derive(Debug)]
pub(super) struct Sheet {
    /// The request reading the lines, `None` when it could not be queued.
    request: Option<GitRequestId>,
    /// One line per session that made a branch, in family order.
    lines: Vec<Line>,
}

/// One session that made a branch, with what git answered about it.
#[derive(Debug)]
struct Line {
    /// The session.
    session: SessionId,
    /// Its title.
    title: String,
    /// The repository it made its branch in.
    repository: Repository,
    /// The branch it made.
    branch: String,
    /// The branch it started from.
    base: String,
    /// The worktree it recorded, `None` in New branch mode.
    worktree: Option<PathBuf>,
    /// What can be removed, `None` while git has not answered.
    offer: Option<Offer>,
}

/// What a line offers to remove.
#[derive(Debug, Default)]
struct Offer {
    worktree: Option<WorktreeCleanup>,
    /// What the worktree had checked out, while it is offered.
    head: Option<Head>,
    branch: Option<BranchCleanup>,
}

/// What a delete removes in one repository, as the sheet showed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Removal {
    /// The project the repository belongs to.
    pub(super) project: ProjectId,
    /// The repository.
    repository: Repository,
    /// What is removed for each session, in family order.
    members: Vec<Removing>,
}

/// What is removed for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Removing {
    worktree: Option<ShownWorktree>,
    branch: Option<ShownBranch>,
}

/// A worktree checked for removal, as the sheet showed it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ShownWorktree {
    /// Where it is, as git listed it.
    path: PathBuf,
    /// What it had checked out.
    head: Head,
    /// How many uncommitted or untracked entries the sheet showed, `None` when git could
    /// not tell.
    changes: Option<usize>,
}

/// A branch checked for deletion, as the sheet showed it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ShownBranch {
    /// The branch name.
    name: String,
    /// The branch the session started it from.
    base: String,
    /// Whether the sheet showed it merged into `base`.
    merged: bool,
}

impl App {
    /// The cleanup lines of the latest [`delete_summary`](Self::delete_summary) for
    /// `session`, as far as git has answered; empty when it has no line or none was asked.
    pub fn cleanup(&self, session: &SessionId) -> Vec<CleanupView> {
        let Some(sheet) = self.cleanups.get(session) else {
            return Vec::new();
        };
        sheet
            .lines
            .iter()
            .map(|line| CleanupView {
                session: line.session.clone(),
                title: line.title.clone(),
                repository: line.repository.name.clone(),
                loading: line.offer.is_none(),
                busy: self.git_work.is_busy(&line.repository.path),
                worktree: line.offer.as_ref().and_then(|offer| offer.worktree.clone()),
                branch: line.offer.as_ref().and_then(|offer| offer.branch.clone()),
            })
            .collect()
    }

    /// Makes the delete sheet of the top-level `session` and queues the reading of its
    /// lines, as [`delete_summary`](Self::delete_summary) describes.
    pub(super) fn read_cleanup(&mut self, session: &SessionId) {
        let mut lines = Vec::new();
        for member in self.family(session) {
            if let Ok(member) = self.session(&member)
                && let Ok(project) = self.project(&member.project)
                && let Some(line) = line(project, member)
            {
                lines.push(line);
            }
        }
        if lines.is_empty() {
            self.cleanups.remove(session);
            return;
        }
        let queries = lines.iter().flat_map(line_queries).collect();
        let work = Work::CleanupCheck {
            session: session.clone(),
        };
        let request = self.git_work.query(&*self.git, work, queries).ok();
        if request.is_none() {
            for line in &mut lines {
                line.offer = Some(Offer::default());
            }
        }
        self.cleanups
            .insert(session.clone(), Sheet { request, lines });
    }

    /// Fills the lines of the delete sheet of `session` with the `answers` git gave to
    /// `request`, unless the sheet waits for another request.
    pub(super) fn cleanup_read(
        &mut self,
        session: &SessionId,
        request: GitRequestId,
        answers: Vec<Result<GitAnswer, GitError>>,
    ) {
        let Some(sheet) = self.cleanups.get_mut(session) else {
            return;
        };
        if sheet.request != Some(request) {
            return;
        }
        let mut answers = answers.into_iter();
        for line in &mut sheet.lines {
            let inspect = answers.next();
            let changes = line.worktree.as_ref().and_then(|_| answers.next());
            let merged = answers.next();
            line.offer = Some(offer(
                line,
                inspect,
                change_count(changes),
                is_merged(merged),
            ));
        }
    }

    /// Queues the fresh reading of every repository of `sheet` where `boxes` check something
    /// the sheet offers, for the removal that follows; returns why a checked line in a busy
    /// repository was kept and the errors of queueing the readings.
    pub(super) fn remove_checked(
        &mut self,
        project: &ProjectId,
        sheet: &Sheet,
        boxes: &[Cleanup],
    ) -> Vec<AppError> {
        let mut failures = Vec::new();
        let mut removals: Vec<Removal> = Vec::new();
        for line in &sheet.lines {
            let Some(offer) = &line.offer else {
                continue;
            };
            let Some(checked) = boxes.iter().find(|boxed| boxed.session == line.session) else {
                continue;
            };
            let Some(removing) = removing(line, offer, checked) else {
                continue;
            };
            if self.git_work.is_busy(&line.repository.path) {
                let repository = line.repository.name.clone();
                let reason = format!("{repository} is busy with another branch change");
                failures.push(AppError::CleanupKept { repository, reason });
                continue;
            }
            match removals
                .iter_mut()
                .find(|removal| removal.repository.path == line.repository.path)
            {
                Some(removal) => removal.members.push(removing),
                None => removals.push(Removal {
                    project: project.clone(),
                    repository: line.repository.clone(),
                    members: vec![removing],
                }),
            }
        }
        for removal in removals {
            let repository = removal.repository.name.clone();
            let held = vec![removal.repository.path.clone()];
            let queries = removal_queries(&removal);
            let work = Work::RemoveCheck(removal);
            if let Err(error) = self.git_work.query_holding(&*self.git, work, queries, held) {
                failures.push(AppError::Cleanup { repository, error });
            }
        }
        failures
    }

    /// Plans `removal` again from the fresh reading `answers` of its repository and queues
    /// the steps that still remove what the sheet showed.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Cleanup`] when the git port cannot queue the removal and
    /// [`AppError::CleanupKept`] when something checked was kept.
    pub(super) fn removal_checked(
        &mut self,
        removal: &Removal,
        answers: Vec<Result<GitAnswer, GitError>>,
    ) -> Result<(), AppError> {
        let repository = removal.repository.name.clone();
        let mut answers = answers.into_iter();
        let info = match answers.next() {
            Some(Ok(GitAnswer::Repo(info))) => info,
            other => {
                return Err(AppError::CleanupKept {
                    repository,
                    reason: unreadable(other),
                });
            }
        };
        let path = &removal.repository.path;
        let mut steps = Vec::new();
        let mut kept = Vec::new();
        for member in &removal.members {
            let mut removed = None;
            if let Some(shown) = &member.worktree {
                if let Some(reason) = worktree_kept(&info, shown, change_count(answers.next())) {
                    kept.push(reason);
                } else {
                    steps.push(GitStep::RemoveWorktree {
                        repository: path.clone(),
                        path: shown.path.clone(),
                    });
                    removed = Some(shown.path.as_path());
                }
            }
            if let Some(shown) = &member.branch {
                let merged = is_merged(answers.next());
                let with_kept_worktree = member
                    .worktree
                    .as_ref()
                    .filter(|_| removed.is_none())
                    .map(|worktree| {
                        let path = worktree.path.display();
                        format!("branch {} stays with the worktree {path}", shown.name)
                    });
                match with_kept_worktree.or_else(|| branch_kept(&info, shown, merged, removed)) {
                    Some(reason) => kept.push(reason),
                    None => steps.push(GitStep::DeleteBranch {
                        repository: path.clone(),
                        branch: shown.name.clone(),
                    }),
                }
            }
        }
        if !steps.is_empty() {
            let work = Work::Remove {
                project: removal.project.clone(),
                repository: repository.clone(),
            };
            if let Err(error) = self.git_work.run(&*self.git, work, steps) {
                if kept.is_empty() {
                    return Err(AppError::Cleanup { repository, error });
                }
                kept.push(format!("nothing else was removed: {error}"));
            }
        }
        if kept.is_empty() {
            Ok(())
        } else {
            Err(AppError::CleanupKept {
                repository,
                reason: kept.join("; "),
            })
        }
    }
}

/// The line of `session` in `project`, `None` for a Checked-out session or one whose
/// repository the project no longer has.
fn line(project: &Project, session: &Session) -> Option<Line> {
    let (branch, base, worktree) = match &session.workspace {
        Workspace::CheckedOut => return None,
        Workspace::NewBranch { branch, base, .. } => (branch, base, None),
        Workspace::Worktree {
            branch, base, path, ..
        } => (branch, base, Some(path.clone())),
    };
    let repository = project
        .repositories
        .iter()
        .find(|repository| match &session.scope {
            Scope::Repository(id) => repository.id == *id,
            Scope::Project => repository.is_root,
        })?;
    Some(Line {
        session: session.id.clone(),
        title: session.title.clone(),
        repository: repository.clone(),
        branch: branch.clone(),
        base: base.clone(),
        worktree,
        offer: None,
    })
}

/// The queries that read `line`: its repository, its worktree's changes when it has one,
/// and whether its branch is merged into its base.
fn line_queries(line: &Line) -> Vec<GitQuery> {
    let repository = line.repository.path.clone();
    let mut queries = vec![GitQuery::Inspect(repository.clone())];
    queries.extend(line.worktree.clone().map(GitQuery::Changes));
    queries.push(GitQuery::IsMerged {
        repository,
        branch: line.branch.clone(),
        base: line.base.clone(),
    });
    queries
}

/// What `line` offers once git answered `inspect` about it, with its worktree's `changes`
/// and whether its branch is `merged`.
fn offer(
    line: &Line,
    inspect: Option<Result<GitAnswer, GitError>>,
    changes: Option<usize>,
    merged: bool,
) -> Offer {
    let Some(Ok(GitAnswer::Repo(info))) = inspect else {
        return Offer::default();
    };
    let listed = line
        .worktree
        .as_deref()
        .and_then(|recorded| listed_worktree(&info, recorded));
    let own = listed.map(|worktree| worktree.path.clone());
    let head = listed.map(|worktree| worktree.head.clone());
    let worktree = own.clone().map(|path| WorktreeCleanup { path, changes });
    let branch = info.has_branch(&line.branch).then(|| {
        let at = info.checked_out_at(&line.branch);
        let in_own_worktree = at.is_some() && at == own.as_deref();
        BranchCleanup {
            name: line.branch.clone(),
            merged,
            checked_out: at.filter(|_| !in_own_worktree).map(Path::to_path_buf),
            in_own_worktree,
            prunable: info.worktrees.iter().any(|worktree| {
                worktree.prunable
                    && matches!(&worktree.head, Head::Branch(name) if *name == line.branch)
            }),
        }
    });
    Offer {
        worktree,
        head,
        branch,
    }
}

/// The worktree git lists for the one recorded at `recorded`, compared as it is and in its
/// canonical form, since git lists real paths; `None` when git lists no such worktree
/// besides the main checkout, or only a prunable one.
fn listed_worktree<'a>(info: &'a RepoInfo, recorded: &Path) -> Option<&'a WorktreeInfo> {
    let canonical = fs::canonicalize(recorded).ok();
    info.worktrees.iter().skip(1).find(|worktree| {
        !worktree.prunable
            && (worktree.path == recorded || canonical.as_ref() == Some(&worktree.path))
    })
}

/// What a delete removes for `line` with the boxes `checked`, `None` when nothing.
fn removing(line: &Line, offer: &Offer, checked: &Cleanup) -> Option<Removing> {
    let worktree = offer
        .worktree
        .as_ref()
        .zip(offer.head.as_ref())
        .filter(|_| checked.remove_worktree)
        .map(|(worktree, head)| ShownWorktree {
            path: worktree.path.clone(),
            head: head.clone(),
            changes: worktree.changes,
        });
    let branch = offer
        .branch
        .as_ref()
        .filter(|branch| {
            checked.delete_branch
                && branch.checked_out.is_none()
                && !branch.prunable
                && (offer.worktree.is_none() || worktree.is_some())
        })
        .map(|branch| ShownBranch {
            name: branch.name.clone(),
            base: line.base.clone(),
            merged: branch.merged,
        });
    (worktree.is_some() || branch.is_some()).then_some(Removing { worktree, branch })
}

/// The queries that read a repository again for `removal`: the repository, then for each
/// session its worktree's changes and whether its branch is merged, for what is removed.
fn removal_queries(removal: &Removal) -> Vec<GitQuery> {
    let repository = removal.repository.path.clone();
    let mut queries = vec![GitQuery::Inspect(repository.clone())];
    for member in &removal.members {
        if let Some(worktree) = &member.worktree {
            queries.push(GitQuery::Changes(worktree.path.clone()));
        }
        if let Some(branch) = &member.branch {
            queries.push(GitQuery::IsMerged {
                repository: repository.clone(),
                branch: branch.name.clone(),
                base: branch.base.clone(),
            });
        }
    }
    queries
}

/// Why the fresh reading of a repository, answered with `answer`, shows nothing.
fn unreadable(answer: Option<Result<GitAnswer, GitError>>) -> String {
    match answer {
        Some(Err(error)) => error.to_string(),
        _ => "git did not describe the repository".into(),
    }
}

/// How many uncommitted or untracked entries a `Changes` `answer` counts, `None` when git
/// could not tell.
fn change_count(answer: Option<Result<GitAnswer, GitError>>) -> Option<usize> {
    answer.and_then(Result::ok).and_then(|answer| match answer {
        GitAnswer::Changes(count) => Some(count),
        GitAnswer::Repo(_) | GitAnswer::Merged(_) => None,
    })
}

/// Whether an `IsMerged` `answer` says merged; a failed check counts as not merged.
fn is_merged(answer: Option<Result<GitAnswer, GitError>>) -> bool {
    answer.is_some_and(|answer| answer == Ok(GitAnswer::Merged(true)))
}

/// Why the worktree `shown` is kept, given the fresh reading `info` and its `changes`;
/// `None` when it still is what the sheet showed.
fn worktree_kept(info: &RepoInfo, shown: &ShownWorktree, changes: Option<usize>) -> Option<String> {
    let path = shown.path.display();
    let listed = info
        .worktrees
        .iter()
        .skip(1)
        .find(|worktree| !worktree.prunable && worktree.path == shown.path);
    match listed {
        None => Some(format!("worktree {path} is no longer there")),
        Some(worktree) if worktree.head != shown.head => {
            Some(format!("worktree {path} has something else checked out"))
        }
        Some(_) if shown.changes.is_some() && changes != shown.changes => Some(match changes {
            Some(count) => format!("worktree {path} has {count} uncommitted files now"),
            None => format!("worktree {path} could not be checked for uncommitted files"),
        }),
        Some(_) => None,
    }
}

/// Why the branch `shown` is kept, given the fresh reading `info` and whether it is
/// `merged`, when the worktree at `removed` is removed before it; `None` when it still is
/// what the sheet showed. A worktree git considers prunable still holds its branch, as git
/// refuses to delete it then.
fn branch_kept(
    info: &RepoInfo,
    shown: &ShownBranch,
    merged: bool,
    removed: Option<&Path>,
) -> Option<String> {
    let name = &shown.name;
    if !info.has_branch(name) {
        return Some(format!("branch {name} is gone"));
    }
    let holding = info.worktrees.iter().find(|worktree| {
        matches!(&worktree.head, Head::Branch(held) if held == name)
            && Some(worktree.path.as_path()) != removed
    });
    if let Some(worktree) = holding {
        let at = worktree.path.display();
        return Some(format!("branch {name} is checked out at {at}"));
    }
    if shown.merged && !merged {
        return Some(format!(
            "branch {name} has commits not merged into {}",
            shown.base
        ));
    }
    None
}

#[cfg(test)]
mod tests;
