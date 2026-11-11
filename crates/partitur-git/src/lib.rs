//! The [`Git`] command port over the `git` command line.
//!
//! [`GitCli`] answers every request on a thread of its own, named after the request
//! (`git-<n>`), so a slow fetch never holds up another request and the caller never waits for
//! a git process. The thread runs the request's queries or steps one after the other and sends
//! its one [`GitEvent`] to the receiver [`GitCli::start`] returned; when that receiver is gone
//! the event is dropped. Dropping the port does not wait for running requests: their threads
//! finish on their own and their events go nowhere.
//!
//! A fetch is stopped once it runs longer than [`GitCli::FETCH_LIMIT`], or the limit set with
//! [`GitCli::with_fetch_limit`], and reported as [`GitError::TimedOut`].

mod cli;
mod porcelain;

use std::sync::Arc;
use std::time::Duration;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use partitur_core::{Git, GitError, GitEvent, GitQuery, GitRequestId, GitStep};

use crate::cli::Runner;

/// The [`Git`] port, running the `git` program on one thread per request.
#[derive(Debug, Clone)]
pub struct GitCli {
    runner: Arc<Runner>,
    events: UnboundedSender<GitEvent>,
}

impl GitCli {
    /// How long a fetch may run before it is stopped.
    pub const FETCH_LIMIT: Duration = Duration::from_secs(60);

    /// Starts the port, running the program `git`; every outcome arrives on the returned
    /// receiver.
    pub fn start() -> (Self, UnboundedReceiver<GitEvent>) {
        let (events, receiver) = unbounded();
        let runner = Runner {
            program: "git".into(),
            env: Vec::new(),
            fetch_limit: Self::FETCH_LIMIT,
        };
        let port = Self {
            runner: Arc::new(runner),
            events,
        };
        (port, receiver)
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
        self.spawn(request, move |runner| {
            let mut done = 0;
            let mut result = Ok(());
            for step in &steps {
                result = runner.step(step);
                if result.is_err() {
                    break;
                }
                done += 1;
            }
            GitEvent::Ran {
                request,
                done,
                result,
            }
        })
    }
}
