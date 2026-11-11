//! The git requests the app has queued and not seen answered yet, each with the surface that
//! asked for it.
//!
//! Every request queued through the [`Git`] port is recorded here as the [`Work`] it serves,
//! with the repositories it changes. When its [`GitEvent`](crate::GitEvent) arrives,
//! [`GitWork::finish`] hands that work back and frees those repositories, so the answer
//! reaches the surface that asked. An answer whose request is not recorded, because nothing
//! waits for it any more, finishes nothing and is ignored.
//!
//! A repository is busy while a request that changes it is in flight. A read-only query makes
//! nothing busy.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::git::{Git, GitError, GitQuery};
use crate::ids::GitRequestId;

/// What a queued git request was made for, so its answer reaches the surface that asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Work {
    /// Reading the repositories of the session being prepared. Its answer goes to the draft
    /// only while the draft still waits for this request.
    DraftInspect,
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
        let request = self.next_request();
        git.query(request, queries)?;
        self.track(request, work, Vec::new());
        Ok(request)
    }

    /// Whether a request in flight changes `repository`.
    pub(super) fn is_busy(&self, repository: &Path) -> bool {
        self.busy.contains_key(repository)
    }

    /// Records `request`, queued for `work`, as changing `repositories` until it finishes.
    pub(super) fn track(&mut self, request: GitRequestId, work: Work, repositories: Vec<PathBuf>) {
        for repository in &repositories {
            *self.busy.entry(repository.clone()).or_default() += 1;
        }
        self.pending.insert(request, (work, repositories));
    }

    /// Takes the request's work and frees its repositories; `None` for a request nothing
    /// waits for.
    pub(super) fn finish(&mut self, request: GitRequestId) -> Option<Work> {
        let (work, repositories) = self.pending.remove(&request)?;
        for repository in repositories {
            let left = self
                .busy
                .get(&repository)
                .map_or(0, |count| count.saturating_sub(1));
            if left == 0 {
                self.busy.remove(&repository);
            } else {
                self.busy.insert(repository, left);
            }
        }
        Some(work)
    }

    /// A request id no earlier request had.
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
        assert_eq!(work.finish(first), Some(Work::DraftInspect));
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
        work.finish(GitRequestId(2));
        assert!(work.is_busy(&api));
        assert!(!work.is_busy(&web));
        work.finish(GitRequestId(1));
        assert!(!work.is_busy(&api));
    }
}
