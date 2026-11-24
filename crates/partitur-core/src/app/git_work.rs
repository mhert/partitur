//! The git requests the app has queued and not seen answered yet, each with the surface that
//! asked for it.
//!
//! Every request queued through the [`Git`] port is recorded here as the [`Work`] it serves,
//! with the repositories it changes. When its [`GitEvent`](crate::GitEvent) arrives,
//! [`GitWork::finish`] hands that work back and frees those repositories, so the answer
//! reaches the surface that asked. An answer whose request is not recorded, because nothing
//! waits for it any more, finishes nothing and is ignored.
//!
//! A repository is busy while a request that changes it is in flight, counted per request, so
//! two requests in one repository keep it busy until both have finished. A read-only query
//! makes nothing busy, unless it holds repositories for the changing work that follows its
//! answer. A create whose sheet is cancelled stays in flight as abandoned work, so
//! its repository stays busy and what it made can still be undone. A create, an approval
//! and a removal a delete asked for keep their repositories busy from their fresh reading
//! until they have run.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::cleanup::Removal;
use super::split::Approving;
use super::workspace::{Planned, workspace_of};
use crate::git::{Git, GitError, GitQuery, GitStep, Head};
use crate::ids::{GitRequestId, ProjectId, ProposalId, SessionId};
use crate::project::Repository;
use crate::session::Workspace;

/// What a queued git request was made for, so its answer reaches the surface that asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Work {
    /// Reading the repositories of the session being prepared. Its answer goes to the draft
    /// only while the draft still waits for this request.
    DraftInspect,
    /// Reading the repository again when Create is pressed, so the steps are planned from
    /// its current state; `planned` is what the sheet showed. Answered into the draft only
    /// while it still waits for this request.
    DraftCheck {
        /// The work as planned from the sheet's earlier reading.
        planned: Planned,
    },
    /// Making the branch or worktree of the session being prepared; answered into the draft
    /// that queued it.
    DraftCreate {
        /// What the request makes.
        planned: Planned,
    },
    /// A create whose sheet was cancelled: what it made is undone.
    Abandoned {
        /// The repository it works in.
        repository: Repository,
        /// What it makes there.
        workspace: Workspace,
        /// What the main checkout had checked out before.
        before: Head,
    },
    /// Reading the repositories of a proposed split's children for its approval card. The
    /// answer for each repository goes to the proposal only while it still waits for this
    /// request there.
    SplitInspect {
        /// The proposal whose card asked.
        proposal: ProposalId,
        /// The repositories read, in the order of the queries.
        repositories: Vec<PathBuf>,
    },
    /// Keeping the repositories of a Create or an approval busy while its fresh reading waits
    /// for the answers about the worktree folders it asked about when pressed. No git request
    /// answers it; it ends when the answers arrive or the Create is cancelled.
    Waiting,
    /// Reading the repositories of an approved split again, so its git work is planned from
    /// their current state; the plans are what the card showed.
    ApprovalCheck(Approving),
    /// Making the branches and worktrees of an approved split, every fetch first.
    Approve {
        /// The approval, with the work planned from the fresh reading.
        approving: Approving,
        /// How many fetch steps come before the steps that make something.
        fetches: usize,
    },
    /// Undoing what a create or an approval made; a failure is reported.
    Undo {
        /// The name of the repository each step restores, in step order.
        repositories: Vec<String>,
    },
    /// Reading the repository of `session`, which works in `folder`, for the Env tab's Git
    /// card. The answer goes to the card only while it is the session's latest reading.
    EnvCheck {
        /// The session whose card asked.
        session: SessionId,
        /// The folder the session works in.
        folder: PathBuf,
    },
    /// Reading what the delete sheet of `session` offers to remove. The answer goes to the
    /// sheet only while it still waits for this request.
    CleanupCheck {
        /// The top-level session whose delete sheet asked.
        session: SessionId,
    },
    /// Reading a repository again when a delete removes what the user checked there, so the
    /// removal is planned from its current state; `removal` is what the sheet showed.
    RemoveCheck(Removal),
    /// Removing what the user checked in one repository; a failure is reported.
    Remove {
        /// The project the repository belongs to.
        project: ProjectId,
        /// The repository's name.
        repository: String,
    },
}

/// The git requests in flight, by request, with the work each serves and the repositories it
/// keeps busy.
#[derive(Debug, Default)]
pub(super) struct GitWork {
    /// The number of the last request queued.
    last: u64,
    pending: HashMap<GitRequestId, (Work, Vec<PathBuf>)>,
    /// How many requests in flight change each repository.
    busy: HashMap<PathBuf, usize>,
}

impl GitWork {
    /// Queues read-only `queries` through `git` for `work`; nothing becomes busy.
    ///
    /// # Errors
    ///
    /// Returns the port's error when it cannot queue the request; nothing is recorded then.
    pub(super) fn query(
        &mut self,
        git: &dyn Git,
        work: Work,
        queries: Vec<GitQuery>,
    ) -> Result<GitRequestId, GitError> {
        self.query_holding(git, work, queries, Vec::new())
    }

    /// Queues read-only `queries` through `git` for `work`, and keeps `held` busy until the
    /// answer arrives, for the changing work queued from it.
    ///
    /// # Errors
    ///
    /// Returns the port's error when it cannot queue the request; nothing is recorded then.
    pub(super) fn query_holding(
        &mut self,
        git: &dyn Git,
        work: Work,
        queries: Vec<GitQuery>,
        held: Vec<PathBuf>,
    ) -> Result<GitRequestId, GitError> {
        let request = self.next_request();
        git.query(request, queries)?;
        self.track(request, work, held);
        Ok(request)
    }

    /// Queues `steps` through `git` for `work`, and marks every repository the steps touch
    /// busy until the request's [`Ran`](crate::GitEvent::Ran) arrives.
    ///
    /// # Errors
    ///
    /// Returns the port's error when it cannot queue the request; nothing is recorded then.
    pub(super) fn run(
        &mut self,
        git: &dyn Git,
        work: Work,
        steps: Vec<GitStep>,
    ) -> Result<GitRequestId, GitError> {
        let request = self.next_request();
        let repositories = steps
            .iter()
            .map(|step| step.repository().to_path_buf())
            .collect();
        git.run(request, steps)?;
        self.track(request, work, repositories);
        Ok(request)
    }

    /// Keeps `repositories` busy, as the request it returns, until it is
    /// [finished](Self::finish); no git request is queued.
    pub(super) fn hold(&mut self, repositories: Vec<PathBuf>) -> GitRequestId {
        let request = self.next_request();
        self.track(request, Work::Waiting, repositories);
        request
    }

    /// Turns the create `request` serves into abandoned work, because its sheet was closed:
    /// once it has run, what it made is undone. Any other request is left as it is.
    pub(super) fn abandon(&mut self, request: GitRequestId) {
        if let Some((work, _)) = self.pending.get_mut(&request)
            && let Work::DraftCreate { planned } = work
        {
            *work = Work::Abandoned {
                repository: planned.repository.clone(),
                workspace: workspace_of(planned),
                before: planned.before.clone(),
            };
        }
    }

    /// Whether a removal a delete asked for is in flight in one of `project`'s repositories,
    /// its fresh reading included.
    pub(super) fn cleaning(&self, project: &ProjectId) -> bool {
        self.pending.values().any(|(work, _)| match work {
            Work::RemoveCheck(removal) => removal.project == *project,
            Work::Remove {
                project: removing, ..
            } => removing == project,
            _ => false,
        })
    }

    /// Whether a request in flight changes `repository`.
    pub(super) fn is_busy(&self, repository: &Path) -> bool {
        self.busy.contains_key(repository)
    }

    /// Records `request`, queued for `work`, as changing `repositories` until it finishes.
    fn track(&mut self, request: GitRequestId, work: Work, repositories: Vec<PathBuf>) {
        for repository in &repositories {
            *self.busy.entry(repository.clone()).or_default() += 1;
        }
        self.pending.insert(request, (work, repositories));
    }

    /// Takes the request's work and frees its repositories; returns the work with the
    /// repositories no request keeps busy any more since, `None` for a request nothing waits
    /// for.
    pub(super) fn finish(&mut self, request: GitRequestId) -> Option<(Work, Vec<PathBuf>)> {
        let (work, repositories) = self.pending.remove(&request)?;
        let mut freed = Vec::new();
        for repository in repositories {
            let left = self
                .busy
                .get(&repository)
                .map_or(0, |count| count.saturating_sub(1));
            if left == 0 {
                self.busy.remove(&repository);
                freed.push(repository);
            } else {
                self.busy.insert(repository, left);
            }
        }
        Some((work, freed))
    }

    /// A request id no earlier request had.
    /// Whether a reading for the Env tab's Git card or the delete sheet of `session` is in
    /// flight.
    #[cfg(test)]
    pub(super) fn reads_for(&self, session: &SessionId) -> bool {
        self.pending.values().any(|(work, _)| {
            matches!(work, Work::EnvCheck { session: asked, .. } | Work::CleanupCheck { session: asked }
                if asked == session)
        })
    }

    /// The holds in flight: the requests that keep repositories busy with no git request.
    #[cfg(test)]
    pub(super) fn holds(&self) -> impl Iterator<Item = GitRequestId> + '_ {
        self.pending
            .iter()
            .filter(|(_, (work, _))| matches!(work, Work::Waiting))
            .map(|(request, _)| *request)
    }

    fn next_request(&mut self) -> GitRequestId {
        self.last += 1;
        GitRequestId(self.last)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::git::GitError;
    use crate::testing::MemoryGit;

    #[test]
    fn a_request_nothing_waits_for_finishes_nothing() {
        let mut work = GitWork::default();

        assert_eq!(work.finish(GitRequestId(7)), None);
    }

    #[test]
    fn the_readings_for_a_session_and_the_holds_are_found() {
        let mut work = GitWork::default();
        let (shown, deleted, other) = (SessionId::new(), SessionId::new(), SessionId::new());
        let folder = PathBuf::from("/p/api");
        work.track(
            GitRequestId(101),
            Work::EnvCheck {
                session: shown.clone(),
                folder: folder.clone(),
            },
            Vec::new(),
        );
        work.track(
            GitRequestId(102),
            Work::CleanupCheck {
                session: deleted.clone(),
            },
            Vec::new(),
        );
        let hold = work.hold(vec![folder]);

        assert!(work.reads_for(&shown));
        assert!(work.reads_for(&deleted));
        assert!(!work.reads_for(&other));
        assert_eq!(work.holds().collect::<Vec<_>>(), [hold]);
    }

    #[test]
    fn a_query_makes_nothing_busy() {
        let git = MemoryGit::new();
        let mut work = GitWork::default();
        let api = PathBuf::from("/p/api");

        let first = work
            .query(
                &git,
                Work::DraftInspect,
                vec![GitQuery::Inspect(api.clone())],
            )
            .unwrap();
        let second = work.query(&git, Work::DraftInspect, Vec::new()).unwrap();

        assert_ne!(first, second);
        assert!(!work.is_busy(&api));
        assert_eq!(git.waiting(), 2);
        assert_eq!(work.finish(first), Some((Work::DraftInspect, Vec::new())));
        assert_eq!(work.finish(first), None);
    }

    #[test]
    fn a_query_the_port_refuses_is_not_remembered() {
        let git = MemoryGit::new();
        let mut work = GitWork::default();
        git.refuse_next_request();

        let refused = work.query(&git, Work::DraftInspect, Vec::new());

        assert!(matches!(refused, Err(GitError::Unavailable(_))));
        assert!(work.pending.is_empty());
    }

    #[test]
    fn a_repository_stays_busy_until_every_request_changing_it_finishes() {
        let mut work = GitWork::default();
        let (api, web) = (PathBuf::from("/p/api"), PathBuf::from("/p/web"));
        work.track(GitRequestId(1), Work::DraftInspect, vec![api.clone()]);
        work.track(
            GitRequestId(2),
            Work::DraftInspect,
            vec![api.clone(), web.clone()],
        );

        assert!(work.is_busy(&api));
        assert_eq!(
            work.finish(GitRequestId(2)),
            Some((Work::DraftInspect, vec![web.clone()]))
        );
        assert!(work.is_busy(&api));
        assert!(!work.is_busy(&web));
        assert_eq!(
            work.finish(GitRequestId(1)),
            Some((Work::DraftInspect, vec![api.clone()]))
        );
        assert!(!work.is_busy(&api));
    }

    fn undo() -> Work {
        Work::Undo {
            repositories: vec!["api".into()],
        }
    }

    fn delete_in(repository: &Path) -> GitStep {
        GitStep::DeleteBranch {
            repository: repository.to_path_buf(),
            branch: "feat/x".into(),
        }
    }

    #[test]
    fn a_run_keeps_its_repositories_busy_until_it_finishes() {
        let git = MemoryGit::new();
        let mut work = GitWork::default();
        let (api, web) = (PathBuf::from("/p/api"), PathBuf::from("/p/web"));

        let request = work.run(&git, undo(), vec![delete_in(&api)]).unwrap();

        assert_eq!(git.waiting(), 1);
        assert!(work.is_busy(&api));
        assert!(!work.is_busy(&web));
        assert_eq!(work.finish(request), Some((undo(), vec![api.clone()])));
        assert!(!work.is_busy(&api));
        assert_eq!(work.finish(request), None);
    }

    #[test]
    fn two_runs_in_one_repository_count_separately() {
        let git = MemoryGit::new();
        let mut work = GitWork::default();
        let api = PathBuf::from("/p/api");

        let first = work.run(&git, undo(), vec![delete_in(&api)]).unwrap();
        let second = work.run(&git, undo(), vec![delete_in(&api)]).unwrap();

        assert_ne!(first, second);
        work.finish(first);
        assert!(work.is_busy(&api));
        work.finish(second);
        assert!(!work.is_busy(&api));
    }

    #[test]
    fn a_run_the_port_refuses_is_not_remembered() {
        let git = MemoryGit::new();
        let mut work = GitWork::default();
        let api = PathBuf::from("/p/api");
        git.refuse_next_request();

        let refused = work.run(&git, undo(), vec![delete_in(&api)]);

        assert!(matches!(refused, Err(GitError::Unavailable(_))));
        assert!(work.pending.is_empty());
        assert!(!work.is_busy(&api));
    }
}
