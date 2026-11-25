//! What deleting a session offers to remove from git, and the removal after the delete.
//!
//! The delete sheet of a top-level session has one line per session of its family that made
//! a worktree or a branch, as its workspace records: a Worktree session offers both, a New
//! branch session its branch, and a session that added a worktree for an existing branch
//! only that worktree. A session that reused an existing branch or worktree, or works in the
//! repository as it was, made nothing and has no line. Its lines are read in the background
//! with one read-only git request: whether the worktree is still there and how many
//! uncommitted files it has, and whether the branch still exists, is merged into the base it
//! started from and is checked out somewhere. Since git lists the real paths of worktrees,
//! the worktrees the lines recorded are resolved through the files port alongside, and a line
//! counts as answered only once both have answered; a worktree that cannot be resolved is
//! compared as it was recorded. Only the answers to the latest reading of a sheet fill it.
//!
//! What a line offers can be shared with sessions outside the family: every other session of
//! the project, archived ones included, since restoring them must find their work. A
//! worktree is in use by those whose folder, or the folder git resolved theirs to, is that
//! worktree, compared as recorded and in its canonical form; a branch by those whose
//! workspace names it in the same repository. A session whose folder has the branch checked
//! out holds it too, through the rules for a checked-out branch below. The sheet names them,
//! in the order the project lists them, and what is in use is never removed, also when the
//! user checked it; the rest of the delete goes ahead. Since a session can start using a
//! worktree or branch after the sheet read it, the sessions left are asked again when the
//! delete queues its removals and again when git has read the repository afresh: what one of
//! them works in or on by then is kept, a worktree's branch with it, and reported.
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

use std::path::{Path, PathBuf};

use super::git_work::Work;
use super::{App, AppError};
use crate::files::{FilesAnswer, FilesError, FilesQuery, FilesRequestId};
use crate::git::{GitAnswer, GitError, GitQuery, GitStep, Head, RepoInfo, WorktreeInfo};
use crate::ids::{GitRequestId, ProjectId, SessionId};
use crate::project::{Project, Repository};
use crate::session::{Scope, Session};
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
    /// The request resolving the worktrees the lines recorded, while it is on its way.
    resolving: Option<FilesRequestId>,
    /// What git answered to `request` while the worktrees were still being resolved.
    held: Option<Vec<Result<GitAnswer, GitError>>>,
    /// One line per session that made a worktree or a branch, in family order.
    lines: Vec<Line>,
}

/// One session that made a worktree or a branch, with what git answered about it.
#[derive(Debug)]
struct Line {
    /// The session.
    session: SessionId,
    /// Its title.
    title: String,
    /// The repository it made its worktree or branch in.
    repository: Repository,
    /// The branch it made and the branch it started it from, `None` when it made none.
    branch: Option<(String, String)>,
    /// The worktree it made, as recorded, `None` when it made none.
    worktree: Option<PathBuf>,
    /// The canonical form of `worktree`, once resolved, `None` when it cannot be resolved.
    canonical: Option<PathBuf>,
    /// The sessions outside the family that may work in `worktree`, in project order.
    outsiders: Vec<Outsider>,
    /// The titles of the sessions outside the family that work on `branch` in `repository`,
    /// in project order.
    branch_users: Vec<String>,
    /// What can be removed, `None` while git has not answered.
    offer: Option<Offer>,
}

/// A session of the project outside the family of a sheet, with the folders it works in:
/// its `cwd` and the one git resolved it to.
#[derive(Debug)]
struct Outsider {
    title: String,
    folders: Vec<PathBuf>,
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
    /// The folders a session working in it has: as git listed it, as the session recorded it
    /// and in its canonical form.
    folders: Vec<PathBuf>,
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
        let Some(sheet) = self.live(session).and_then(|live| live.cleanup.as_ref()) else {
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
        let family = self.family(session);
        let mut lines = Vec::new();
        for member in &family {
            if let Ok(member) = self.session(member)
                && let Ok(project) = self.project(&member.project)
            {
                let outsiders: Vec<&Session> = self
                    .sessions
                    .iter()
                    .filter(|other| other.project == member.project && !family.contains(&other.id))
                    .collect();
                lines.extend(line(project, member, &outsiders));
            }
        }
        if lines.is_empty() {
            if let Some(live) = self.live_mut(session) {
                live.cleanup = None;
            }
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
        let worktrees: Vec<PathBuf> = lines
            .iter()
            .filter_map(|line| line.worktree.clone())
            .collect();
        let resolving = (request.is_some() && !worktrees.is_empty()).then(|| {
            let resolving = self.files_request();
            self.files
                .query(resolving, vec![FilesQuery::Resolve(worktrees)]);
            resolving
        });
        self.live_entry(session).cleanup = Some(Sheet {
            request,
            resolving,
            held: None,
            lines,
        });
    }

    /// Takes the canonical forms of the recorded worktrees the files port resolved for
    /// `request`, and fills the sheet's lines when git has answered already; `None` then.
    /// Answers no sheet waits for are returned back untouched.
    pub(super) fn cleanup_resolved(
        &mut self,
        request: FilesRequestId,
        answers: Vec<Result<FilesAnswer, FilesError>>,
    ) -> Option<Vec<Result<FilesAnswer, FilesError>>> {
        let Some(sheet) = self
            .live
            .values_mut()
            .filter_map(|live| live.cleanup.as_mut())
            .find(|sheet| sheet.resolving == Some(request))
        else {
            return Some(answers);
        };
        let resolved = match answers.into_iter().next() {
            Some(Ok(FilesAnswer::Resolved(resolved))) => resolved,
            _ => Vec::new(),
        };
        for line in &mut sheet.lines {
            line.canonical = line.worktree.as_ref().and_then(|worktree| {
                resolved
                    .iter()
                    .find(|fact| fact.given == *worktree)
                    .and_then(|fact| fact.canonical.clone())
            });
        }
        sheet.resolving = None;
        if let Some(answers) = sheet.held.take() {
            sheet.fill(answers);
        }
        None
    }

    /// Fills the lines of the delete sheet of `session` with the `answers` git gave to
    /// `request`, unless the sheet waits for another request.
    pub(super) fn cleanup_read(
        &mut self,
        session: &SessionId,
        request: GitRequestId,
        answers: Vec<Result<GitAnswer, GitError>>,
    ) {
        let Some(sheet) = self
            .live_mut(session)
            .and_then(|live| live.cleanup.as_mut())
        else {
            return;
        };
        if sheet.request != Some(request) {
            return;
        }
        if sheet.resolving.is_some() {
            sheet.held = Some(answers);
            return;
        }
        sheet.fill(answers);
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
            let Some(mut removing) = removing(line, offer, checked) else {
                continue;
            };
            let held = self.hold_back(project, &line.repository, &mut removing);
            if !held.is_empty() {
                failures.push(AppError::CleanupKept {
                    repository: line.repository.name.clone(),
                    reason: held.join("; "),
                });
            }
            if removing.worktree.is_none() && removing.branch.is_none() {
                continue;
            }
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
                let changes = change_count(answers.next());
                let reason = self
                    .worktree_held(&removal.project, shown)
                    .or_else(|| worktree_kept(&info, shown, changes));
                if let Some(reason) = reason {
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
                let reason = with_kept_worktree
                    .or_else(|| self.branch_held(&removal.project, &removal.repository, shown))
                    .or_else(|| branch_kept(&info, shown, merged, removed));
                match reason {
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

impl App {
    /// Takes out of `removing` in `repository` of `project` what a session of the project
    /// works in or on now, the branch of a worktree kept with it, and returns why. The deleted
    /// family is gone by then, so every session left counts.
    fn hold_back(
        &self,
        project: &ProjectId,
        repository: &Repository,
        removing: &mut Removing,
    ) -> Vec<String> {
        let mut held = Vec::new();
        if let Some(shown) = &removing.worktree
            && let Some(reason) = self.worktree_held(project, shown)
        {
            held.push(reason);
            if let Some(branch) = removing.branch.take() {
                let path = shown.path.display();
                held.push(format!(
                    "branch {} stays with the worktree {path}",
                    branch.name
                ));
            }
            removing.worktree = None;
        }
        if let Some(shown) = &removing.branch
            && let Some(reason) = self.branch_held(project, repository, shown)
        {
            held.push(reason);
            removing.branch = None;
        }
        held
    }

    /// Why the worktree `shown` is kept because a session of `project` works in it now,
    /// `None` when none does.
    fn worktree_held(&self, project: &ProjectId, shown: &ShownWorktree) -> Option<String> {
        let users = self.sessions.iter().filter(|session| {
            session.project == *project
                && std::iter::once(&session.cwd)
                    .chain(&session.resolved_cwd)
                    .any(|folder| shown.folders.contains(folder))
        });
        let path = shown.path.display();
        in_use_by(users).map(|users| format!("worktree {path} is {users}"))
    }

    /// Why the branch `shown` of `repository` is kept because a session of `project` works
    /// on it now, `None` when none does.
    fn branch_held(
        &self,
        project: &ProjectId,
        repository: &Repository,
        shown: &ShownBranch,
    ) -> Option<String> {
        let known = self.project(project).ok()?;
        let users = self.sessions.iter().filter(|session| {
            session.project == *project
                && session.workspace.branch() == Some(shown.name.as_str())
                && repository_of(known, session).is_some_and(|at| at.id == repository.id)
        });
        in_use_by(users).map(|users| format!("branch {} is {users}", shown.name))
    }
}

/// `in use by "<title>"` for the first of `users`, with ` and <n> more` for more than one;
/// `None` for none.
fn in_use_by<'a>(mut users: impl Iterator<Item = &'a Session>) -> Option<String> {
    let first = users.next()?;
    Some(match users.count() {
        0 => format!("in use by \"{}\"", first.title),
        more => format!("in use by \"{}\" and {more} more", first.title),
    })
}

impl Sheet {
    /// Fills the lines with the `answers` git gave to the sheet's request.
    fn fill(&mut self, answers: Vec<Result<GitAnswer, GitError>>) {
        let mut answers = answers.into_iter();
        for line in &mut self.lines {
            let inspect = answers.next();
            let changes = line.worktree.as_ref().and_then(|_| answers.next());
            let merged = line.branch.as_ref().and_then(|_| answers.next());
            line.offer = Some(offer(
                line,
                inspect,
                change_count(changes),
                is_merged(merged),
            ));
        }
    }
}

/// The line of `session` in `project`, with the sessions `outsiders` of the project outside
/// its family that may hold back what it offers; `None` for a session that made neither a
/// worktree nor a branch, or one whose repository the project no longer has.
fn line(project: &Project, session: &Session, outsiders: &[&Session]) -> Option<Line> {
    let workspace = &session.workspace;
    let branch = workspace
        .made_branch()
        .zip(workspace.base())
        .map(|(branch, base)| (branch.to_owned(), base.to_owned()));
    let worktree = workspace.made_worktree().map(Path::to_path_buf);
    if branch.is_none() && worktree.is_none() {
        return None;
    }
    let repository = repository_of(project, session)?;
    let branch_users = branch
        .as_ref()
        .map(|(name, _)| {
            outsiders
                .iter()
                .filter(|other| {
                    other.workspace.branch() == Some(name.as_str())
                        && repository_of(project, other).is_some_and(|at| at.id == repository.id)
                })
                .map(|other| other.title.clone())
                .collect()
        })
        .unwrap_or_default();
    let outsiders = outsiders
        .iter()
        .map(|other| Outsider {
            title: other.title.clone(),
            folders: std::iter::once(&other.cwd)
                .chain(&other.resolved_cwd)
                .cloned()
                .collect(),
        })
        .collect();
    Some(Line {
        session: session.id.clone(),
        title: session.title.clone(),
        repository: repository.clone(),
        branch,
        worktree,
        canonical: None,
        outsiders,
        branch_users,
        offer: None,
    })
}

/// The repository of `project` that `session` works in, `None` when the project no longer
/// has it.
fn repository_of<'a>(project: &'a Project, session: &Session) -> Option<&'a Repository> {
    project
        .repositories
        .iter()
        .find(|repository| match &session.scope {
            Scope::Repository(id) => repository.id == *id,
            Scope::Project => repository.is_root,
        })
}

impl Line {
    /// The titles of the sessions outside the family that work in the line's worktree,
    /// compared as it was recorded and in its canonical form, like git's listing of it.
    fn worktree_users(&self) -> Vec<String> {
        let Some(recorded) = &self.worktree else {
            return Vec::new();
        };
        self.outsiders
            .iter()
            .filter(|other| {
                other
                    .folders
                    .iter()
                    .any(|folder| folder == recorded || self.canonical.as_ref() == Some(folder))
            })
            .map(|other| other.title.clone())
            .collect()
    }
}

/// The queries that read `line`: its repository, its worktree's changes when it has one,
/// and whether its branch is merged into its base.
fn line_queries(line: &Line) -> Vec<GitQuery> {
    let repository = line.repository.path.clone();
    let mut queries = vec![GitQuery::Inspect(repository.clone())];
    queries.extend(line.worktree.clone().map(GitQuery::Changes));
    queries.extend(
        line.branch
            .clone()
            .map(|(branch, base)| GitQuery::IsMerged {
                repository,
                branch,
                base,
            }),
    );
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
        .and_then(|recorded| listed_worktree(&info, recorded, line.canonical.as_ref()));
    let own = listed.map(|worktree| worktree.path.clone());
    let head = listed.map(|worktree| worktree.head.clone());
    let worktree = own.clone().map(|path| WorktreeCleanup {
        path,
        changes,
        used_by: line.worktree_users(),
    });
    let made = line.branch.as_ref().map(|(name, _)| name);
    let branch = made.filter(|name| info.has_branch(name)).map(|name| {
        let at = info.checked_out_at(name);
        let in_own_worktree = at.is_some() && at == own.as_deref();
        BranchCleanup {
            name: name.clone(),
            merged,
            checked_out: at.filter(|_| !in_own_worktree).map(Path::to_path_buf),
            in_own_worktree,
            prunable: info.worktrees.iter().any(|worktree| {
                worktree.prunable
                    && matches!(&worktree.head, Head::Branch(branch) if branch == name)
            }),
            used_by: line.branch_users.clone(),
        }
    });
    Offer {
        worktree,
        head,
        branch,
    }
}

/// The worktree git lists for the one recorded at `recorded`, compared as it is and in its
/// `canonical` form, since git lists real paths; `None` when git lists no such worktree
/// besides the main checkout, or only a prunable one.
fn listed_worktree<'a>(
    info: &'a RepoInfo,
    recorded: &Path,
    canonical: Option<&PathBuf>,
) -> Option<&'a WorktreeInfo> {
    info.worktrees.iter().skip(1).find(|worktree| {
        !worktree.prunable && (worktree.path == recorded || canonical == Some(&worktree.path))
    })
}

/// What a delete removes for `line` with the boxes `checked`, `None` when nothing.
fn removing(line: &Line, offer: &Offer, checked: &Cleanup) -> Option<Removing> {
    let worktree = offer
        .worktree
        .as_ref()
        .zip(offer.head.as_ref())
        .filter(|(worktree, _)| checked.remove_worktree && worktree.used_by.is_empty())
        .map(|(worktree, head)| ShownWorktree {
            path: worktree.path.clone(),
            head: head.clone(),
            changes: worktree.changes,
            folders: std::iter::once(&worktree.path)
                .chain(&line.worktree)
                .chain(&line.canonical)
                .cloned()
                .collect(),
        });
    let branch = offer
        .branch
        .as_ref()
        .zip(line.branch.as_ref())
        .filter(|(branch, _)| {
            checked.delete_branch
                && branch.checked_out.is_none()
                && !branch.prunable
                && branch.used_by.is_empty()
                && (offer.worktree.is_none() || worktree.is_some())
        })
        .map(|(branch, (_, base))| ShownBranch {
            name: branch.name.clone(),
            base: base.clone(),
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
        GitAnswer::Repo(_) | GitAnswer::Merged(_) | GitAnswer::Folder(_) => None,
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
