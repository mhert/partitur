//! The [`Git`] command port over the `git` command line.
//!
//! [`GitCli`] answers every request on a thread of its own, named after the request
//! (`git-<n>`), so a slow fetch never holds up a query or a change to another repository, and
//! the caller never waits for a git process. The thread runs the request's queries or steps
//! one after the other and sends its one [`GitEvent`] to the receiver [`GitCli::start`]
//! returned; when that receiver is gone the event is dropped. Dropping the port does not wait
//! for running requests: their threads finish on their own and their events go nowhere.
//!
//! Git runs in the [`ProcessEnv`] given to [`GitCli::with_process_env`], which may still be
//! pending; a request's thread waits for it, so the caller never does.
//!
//! A fetch is stopped once it runs longer than [`GitCli::FETCH_LIMIT`], or the limit set with
//! [`GitCli::with_fetch_limit`], and every command of a query once it runs longer than
//! [`GitCli::READ_LIMIT`], or the limit set with [`GitCli::with_read_limit`]; either is
//! reported as [`GitError::TimedOut`].
//!
//! The steps of [`Git::run`] requests that change one repository run one request after
//! another: before its first step a request takes a lock for every repository its steps name,
//! in a fixed order so two requests never wait on each other, and holds them until its last
//! step ended. A repository is known by its canonical path, so a symbolic link or a trailing
//! separator names the same one, and by the path as given when that cannot be resolved. A
//! [`Git::query`] takes no lock and never waits for a running step.

mod cli;
mod porcelain;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use partitur_core::{Git, GitError, GitEvent, GitQuery, GitRequestId, GitStep, ProcessEnv};

use crate::cli::Runner;

/// The [`Git`] port, running the `git` program on one thread per request.
#[derive(Debug, Clone)]
pub struct GitCli {
    runner: Arc<Runner>,
    events: UnboundedSender<GitEvent>,
    locks: Arc<RepoLocks>,
}

/// One lock per repository a [`Git::run`] request changed, by its canonical path.
#[derive(Debug, Default)]
struct RepoLocks(Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>);

impl RepoLocks {
    /// The locks of `repositories`, one per repository, sorted by its canonical path.
    fn of<'a>(&self, repositories: impl IntoIterator<Item = &'a Path>) -> Vec<Arc<Mutex<()>>> {
        let keys: BTreeSet<PathBuf> = repositories
            .into_iter()
            .map(|path| partitur_core::canonical(path).unwrap_or_else(|_| path.to_path_buf()))
            .collect();
        // A thread that panicked while it held the table left it whole: it only ever adds.
        let mut table = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        keys.into_iter()
            .map(|key| Arc::clone(table.entry(key).or_default()))
            .collect()
    }
}

/// Runs `work` while holding every one of `locks`, taken in their order. A lock whose holder
/// panicked is taken all the same, so one failed request never blocks a repository for good.
fn with_held<T>(locks: &[Arc<Mutex<()>>], work: impl FnOnce() -> T) -> T {
    let _held: Vec<MutexGuard<'_, ()>> = locks
        .iter()
        .map(|lock| lock.lock().unwrap_or_else(PoisonError::into_inner))
        .collect();
    work()
}

impl GitCli {
    /// How long a fetch may run before it is stopped.
    pub const FETCH_LIMIT: Duration = Duration::from_secs(60);

    /// How long one command of a query may run before it is stopped.
    pub const READ_LIMIT: Duration = Duration::from_secs(30);

    /// Starts the port, running the program `git`; every outcome arrives on the returned
    /// receiver.
    pub fn start() -> (Self, UnboundedReceiver<GitEvent>) {
        let (events, receiver) = unbounded();
        let runner = Runner {
            program: "git".into(),
            base: ProcessEnv::ready(Vec::new()),
            env: Vec::new(),
            fetch_limit: Self::FETCH_LIMIT,
            read_limit: Self::READ_LIMIT,
        };
        let port = Self {
            runner: Arc::new(runner),
            events,
            locks: Arc::default(),
        };
        (port, receiver)
    }

    /// Runs every git process in `env`, added to the inherited environment. A request waits
    /// on its own thread until `env` is known, so the caller never waits.
    #[must_use]
    pub fn with_process_env(self, env: ProcessEnv) -> Self {
        let mut runner = Arc::unwrap_or_clone(self.runner);
        runner.base = env;
        Self {
            runner: Arc::new(runner),
            ..self
        }
    }

    /// Runs every git process with these environment variables added, replacing any set
    /// before, as when tests keep git away from the user's configuration.
    #[must_use]
    pub fn with_env(self, env: Vec<(String, String)>) -> Self {
        let mut runner = Arc::unwrap_or_clone(self.runner);
        runner.env = env;
        Self {
            runner: Arc::new(runner),
            ..self
        }
    }

    /// Stops a fetch once it runs longer than `limit`, instead of [`Self::FETCH_LIMIT`].
    #[must_use]
    pub fn with_fetch_limit(self, limit: Duration) -> Self {
        let mut runner = Arc::unwrap_or_clone(self.runner);
        runner.fetch_limit = limit;
        Self {
            runner: Arc::new(runner),
            ..self
        }
    }

    /// Stops a command of a query once it runs longer than `limit`, instead of
    /// [`Self::READ_LIMIT`].
    #[must_use]
    pub fn with_read_limit(self, limit: Duration) -> Self {
        let mut runner = Arc::unwrap_or_clone(self.runner);
        runner.read_limit = limit;
        Self {
            runner: Arc::new(runner),
            ..self
        }
    }

    /// Runs `work` on a new thread named after `request` and sends the event it returns.
    fn spawn(
        &self,
        request: GitRequestId,
        work: impl FnOnce(&Runner) -> GitEvent + Send + 'static,
    ) -> Result<(), GitError> {
        let runner = Arc::clone(&self.runner);
        let events = self.events.clone();
        std::thread::Builder::new()
            .name(request.to_string())
            .spawn(move || {
                // A receiver that is gone wants no more outcomes.
                let _ = events.unbounded_send(work(&runner));
            })
            .map(drop)
            .map_err(|error| GitError::Unavailable(error.to_string()))
    }
}

impl Git for GitCli {
    fn query(&self, request: GitRequestId, queries: Vec<GitQuery>) -> Result<(), GitError> {
        self.spawn(request, move |runner| GitEvent::Answered {
            request,
            answers: queries.iter().map(|query| runner.query(query)).collect(),
        })
    }

    fn run(&self, request: GitRequestId, steps: Vec<GitStep>) -> Result<(), GitError> {
        let locks = Arc::clone(&self.locks);
        self.spawn(request, move |runner| {
            let held = locks.of(steps.iter().map(GitStep::repository));
            let (done, result) = with_held(&held, || {
                let mut done = 0;
                for step in &steps {
                    if let Err(error) = runner.step(step) {
                        return (done, Err(error));
                    }
                    done += 1;
                }
                (done, Ok(()))
            });
            GitEvent::Ran {
                request,
                done,
                result,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::path::Path;
    use std::sync::Arc;

    use super::{RepoLocks, with_held};

    #[test]
    fn one_repository_named_twice_has_one_lock() {
        let locks = RepoLocks::default();
        let dir = tempfile::TempDir::new().expect("a temporary folder");
        let mut slashed = dir.path().as_os_str().to_owned();
        slashed.push("/");

        let held = locks.of([dir.path(), Path::new(&slashed)]);

        assert_eq!(held.len(), 1);
    }

    #[test]
    fn locks_come_sorted_and_are_shared_between_requests() {
        let locks = RepoLocks::default();
        let first = locks.of([Path::new("/nowhere/b"), Path::new("/nowhere/a")]);
        let again = locks.of([Path::new("/nowhere/a")]);

        assert_eq!(first.len(), 2);
        assert!(Arc::ptr_eq(&first[0], &again[0]), "a sorts before b");
        assert!(!Arc::ptr_eq(&first[1], &again[0]));
    }

    #[test]
    fn a_lock_whose_holder_panicked_is_taken_all_the_same() {
        let locks = RepoLocks::default();
        let held = locks.of([Path::new("/nowhere/a")]);
        let poisoned = Arc::clone(&held[0]);
        let _ = std::thread::spawn(move || {
            let _guard = poisoned.lock().expect("the lock is free");
            panic!("the request failed");
        })
        .join();
        assert!(held[0].is_poisoned());

        assert_eq!(with_held(&held, || 7), 7);
    }

    #[test]
    fn the_work_runs_while_the_locks_are_held() {
        let locks = RepoLocks::default();
        let held = locks.of([Path::new("/nowhere/a")]);

        with_held(&held, || assert!(held[0].try_lock().is_err()));
        assert!(held[0].try_lock().is_ok());
    }
}
