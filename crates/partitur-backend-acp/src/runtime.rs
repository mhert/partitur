//! The [`AcpBackend`]: a tokio runtime on its own thread that owns one adapter process per
//! session.
//!
//! [`Backend`] methods only queue a [`Command`] and return. The runtime thread routes each
//! command to the session it names, and every session's connection reports back through the
//! event channel handed out by [`AcpBackend::start`]. Only this crate names tokio: the event
//! receiver is a plain `futures` stream.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use futures::channel::mpsc as event_channel;
use partitur_core::{
    Backend, BackendError, BackendEvent, BackendSessionId, ConfigId, ConfigValueId, PermissionId,
    PermissionOptionId, PromptContent, RequestId, SessionId,
};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::task::{self, JoinError, JoinSet};
use tokio::time::{Instant, sleep_until};

use crate::connection::{self, Open, SessionCommand, SessionStart};

/// Names the runtime thread and its workers, so they are recognizable in a debugger.
const THREAD_NAME: &str = "partitur-acp";
/// Sessions spend their time waiting on pipes; two workers keep one busy session from
/// stalling the others.
const WORKER_THREADS: usize = 2;
/// How long shutdown waits for sessions to close their adapters before abandoning them.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// The sending half of the event channel every session reports through.
pub(crate) type Events = event_channel::UnboundedSender<BackendEvent>;

/// The sessions that have been created or loaded and whose connection is still up.
///
/// A session joins the set just before its created or loaded event is sent and leaves it
/// when its connection ends, so [`AcpBackend`] can reject commands for unknown sessions
/// synchronously.
pub(crate) type LiveSessions = Arc<Mutex<HashSet<SessionId>>>;

/// Locks `mutex`, recovering the data when another thread panicked while holding it: every
/// value guarded in this crate stays consistent between statements.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Sends `event` to the core; an event nobody listens for any more is dropped.
pub(crate) fn emit(events: &Events, event: BackendEvent) {
    if events.unbounded_send(event).is_err() {
        tracing::debug!("the event receiver is gone, dropping an event");
    }
}

/// How the agent's reasoning reaches Partitur.
///
/// Sets `CLAUDE_CODE_THINKING_DISPLAY` on every adapter process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThinkingDisplay {
    /// Reasoning arrives as summarized thought chunks.
    #[default]
    Summarized,
    /// Reasoning is not sent.
    Omitted,
}

impl ThinkingDisplay {
    /// The value of `CLAUDE_CODE_THINKING_DISPLAY` for this setting.
    pub(crate) fn env_value(self) -> &'static str {
        match self {
            Self::Summarized => "summarized",
            Self::Omitted => "omitted",
        }
    }
}

/// How to start the ACP adapter process, once per session.
#[derive(Debug, Clone)]
pub struct AcpConfig {
    /// The program to run.
    pub command: PathBuf,
    /// The program's arguments.
    pub args: Vec<String>,
    /// Extra environment variables for the process, on top of the inherited environment.
    pub env: Vec<(String, String)>,
    /// How the agent's reasoning is reported.
    pub thinking_display: ThinkingDisplay,
}

impl Default for AcpConfig {
    /// Runs the Claude Code adapter through `npx`, with summarized thinking.
    fn default() -> Self {
        Self {
            command: PathBuf::from("npx"),
            args: vec![
                "--yes".to_owned(),
                "@agentclientprotocol/claude-agent-acp".to_owned(),
            ],
            env: Vec::new(),
            thinking_display: ThinkingDisplay::Summarized,
        }
    }
}

/// Why the backend could not start.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AcpError {
    /// The runtime thread could not be spawned.
    #[error("failed to spawn the ACP runtime thread")]
    Spawn(#[source] io::Error),
    /// The async runtime could not be built.
    #[error("failed to build the ACP runtime")]
    Runtime(#[source] io::Error),
}

/// A request from [`AcpBackend`] to its runtime thread.
pub(crate) enum Command {
    /// Starts an adapter process and creates or loads a session in it.
    Open(SessionStart),
    /// A command for one open session.
    Session {
        /// The session the command is for.
        session: SessionId,
        /// What to do.
        command: SessionCommand,
    },
    /// Closes every session and stops the runtime.
    Shutdown,
}

/// The ACP [`Backend`]: one adapter process per session, driven from a runtime thread.
///
/// Dropping the backend shuts it down like [`AcpBackend::shutdown`].
#[derive(Debug)]
pub struct AcpBackend {
    commands: mpsc::UnboundedSender<Command>,
    live: LiveSessions,
    thread: Option<JoinHandle<()>>,
}

impl AcpBackend {
    /// Starts the runtime thread. No adapter process runs until a session is created or
    /// loaded.
    ///
    /// Returns the backend and the receiver of every [`BackendEvent`] it reports. The
    /// receiver is a `futures` stream and needs no particular async runtime.
    ///
    /// ```no_run
    /// use std::path::Path;
    ///
    /// use partitur_backend_acp::{AcpBackend, AcpConfig};
    /// use partitur_core::{Backend, RequestId, SessionId};
    ///
    /// let (backend, events) = AcpBackend::start(AcpConfig::default())?;
    /// backend.new_session(RequestId(1), SessionId::new(), Path::new("/work"))?;
    /// // `events` yields `SessionCreated`, then the session's updates.
    /// # drop(events);
    /// backend.shutdown();
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`AcpError::Runtime`] when the async runtime cannot be built and
    /// [`AcpError::Spawn`] when its thread cannot be spawned.
    pub fn start(
        config: AcpConfig,
    ) -> Result<(Self, event_channel::UnboundedReceiver<BackendEvent>), AcpError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKER_THREADS)
            .thread_name(THREAD_NAME)
            .enable_all()
            .build()
            .map_err(AcpError::Runtime)?;
        let (commands, command_receiver) = mpsc::unbounded_channel();
        let (event_sender, event_receiver) = event_channel::unbounded();
        let live = LiveSessions::default();
        let thread = std::thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn({
                let live = Arc::clone(&live);
                move || {
                    runtime.block_on(run(command_receiver, Arc::new(config), event_sender, live));
                }
            })
            .map_err(AcpError::Spawn)?;
        Ok((
            Self {
                commands,
                live,
                thread: Some(thread),
            },
            event_receiver,
        ))
    }

    /// Cancels every session's turn, ends every adapter process and waits for the runtime
    /// thread to finish.
    pub fn shutdown(mut self) {
        self.stop();
    }

    /// Asks the runtime to shut down and waits for its thread; does nothing the second time.
    ///
    /// The wait is bounded: the runtime gives its sessions [`SHUTDOWN_GRACE`] and then drops
    /// them, which kills their processes.
    fn stop(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        // The runtime may already be gone; then there is nothing left to stop.
        let _ = self.commands.send(Command::Shutdown);
        let _ = thread
            .join()
            .inspect_err(|_| tracing::error!("the ACP runtime thread panicked"));
    }

    /// Queues `command` for the runtime.
    fn send(&self, command: Command) -> Result<(), BackendError> {
        self.commands.send(command).map_err(|_| unavailable())
    }

    /// Queues `command` for `session`, which must be live.
    fn send_to(&self, session: SessionId, command: SessionCommand) -> Result<(), BackendError> {
        if self.commands.is_closed() {
            return Err(unavailable());
        }
        if !lock(&self.live).contains(&session) {
            return Err(BackendError::UnknownSession(session));
        }
        self.send(Command::Session { session, command })
    }
}

/// The error for every command once the runtime has stopped.
fn unavailable() -> BackendError {
    BackendError::Unavailable("the ACP runtime has stopped".to_owned())
}

impl Drop for AcpBackend {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Backend for AcpBackend {
    fn new_session(
        &self,
        request: RequestId,
        session: SessionId,
        cwd: &Path,
    ) -> Result<(), BackendError> {
        self.send(Command::Open(SessionStart {
            request,
            session,
            open: Open::New,
            cwd: cwd.to_path_buf(),
        }))
    }

    fn load_session(
        &self,
        request: RequestId,
        session: SessionId,
        backend_session: &BackendSessionId,
        cwd: &Path,
    ) -> Result<(), BackendError> {
        self.send(Command::Open(SessionStart {
            request,
            session,
            open: Open::Load(backend_session.clone()),
            cwd: cwd.to_path_buf(),
        }))
    }

    fn prompt(&self, session: SessionId, content: Vec<PromptContent>) -> Result<(), BackendError> {
        self.send_to(session, SessionCommand::Prompt(content))
    }

    fn cancel(&self, session: SessionId) -> Result<(), BackendError> {
        self.send_to(session, SessionCommand::Cancel)
    }

    fn set_config(
        &self,
        session: SessionId,
        option: &ConfigId,
        value: &ConfigValueId,
    ) -> Result<(), BackendError> {
        self.send_to(
            session,
            SessionCommand::SetConfig {
                option: option.clone(),
                value: value.clone(),
            },
        )
    }

    /// Queues the answer; an answer to a request that is no longer open is logged and
    /// dropped by the session, since the user may answer after the turn ended.
    fn respond_permission(
        &self,
        session: SessionId,
        permission: PermissionId,
        option: Option<PermissionOptionId>,
    ) -> Result<(), BackendError> {
        self.send_to(
            session,
            SessionCommand::RespondPermission { permission, option },
        )
    }
}

/// The runtime's main loop: spawns a task per opened session, routes commands to it and
/// settles tasks as they end. After [`Command::Shutdown`] (or once the backend is gone) it
/// closes every session and waits up to [`SHUTDOWN_GRACE`] for their tasks.
async fn run(
    mut commands: mpsc::UnboundedReceiver<Command>,
    config: Arc<AcpConfig>,
    events: Events,
    live: LiveSessions,
) {
    let mut sessions = HashMap::new();
    let mut tasks = JoinSet::new();
    let mut owners = HashMap::new();
    // Set once shutdown began: the moment the remaining sessions are abandoned.
    let mut deadline: Option<Instant> = None;
    loop {
        if deadline.is_some() && tasks.is_empty() {
            break;
        }
        tokio::select! {
            Some(ended) = tasks.join_next_with_id() => {
                settle(ended, &mut owners, &mut sessions, &live, &events);
            }
            command = commands.recv(), if deadline.is_none() => match command {
                Some(Command::Open(start)) => {
                    let (sender, receiver) = mpsc::unbounded_channel();
                    // Replacing an older connection of the same session drops its sender,
                    // which closes that connection.
                    sessions.insert(start.session.clone(), sender);
                    let owner = (start.session.clone(), start.request);
                    let task = tasks.spawn(connection::run_session(
                        Arc::clone(&config),
                        start,
                        receiver,
                        events.clone(),
                        Arc::clone(&live),
                    ));
                    owners.insert(task.id(), owner);
                }
                Some(Command::Session { session, command }) => {
                    route(&mut sessions, &session, command);
                }
                Some(Command::Shutdown) | None => {
                    close_all(&mut sessions);
                    deadline = Some(Instant::now() + SHUTDOWN_GRACE);
                }
            },
            () = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                tracing::warn!("ACP sessions did not close in time, killing their processes");
                break;
            }
        }
    }
}

/// Cancels every session's turn and drops its route, which ends its command loop and closes
/// its adapter.
fn close_all(sessions: &mut HashMap<SessionId, mpsc::UnboundedSender<SessionCommand>>) {
    for (_, sender) in sessions.drain() {
        // A session whose connection already ended has nothing to cancel.
        let _ = sender.send(SessionCommand::Cancel);
    }
}

/// Settles a session task that ended. A task that returned has already reported its end;
/// one that panicked or was aborted is logged, forgotten, and reported to the core like a
/// connection that broke: a closed session once it was reported, else a failed request.
fn settle(
    ended: Result<(task::Id, ()), JoinError>,
    owners: &mut HashMap<task::Id, (SessionId, RequestId)>,
    sessions: &mut HashMap<SessionId, mpsc::UnboundedSender<SessionCommand>>,
    live: &LiveSessions,
    events: &Events,
) {
    let (id, failure) = match ended {
        Ok((id, ())) => (id, None),
        Err(error) => (error.id(), Some(error)),
    };
    let Some((session, request)) = owners.remove(&id) else {
        return;
    };
    let Some(failure) = failure else { return };
    tracing::error!(%session, error = %failure, "an ACP session task ended abnormally");
    // A newer connection of the same session keeps its route.
    if sessions
        .get(&session)
        .is_some_and(mpsc::UnboundedSender::is_closed)
    {
        sessions.remove(&session);
    }
    let error = format!("the session task failed: {failure}");
    let event = if lock(live).remove(&session) {
        BackendEvent::SessionClosed { session, error }
    } else {
        BackendEvent::RequestFailed {
            request,
            session,
            error,
        }
    };
    emit(events, event);
}

/// Hands `command` to `session`'s connection, forgetting sessions whose connection ended.
fn route(
    sessions: &mut HashMap<SessionId, mpsc::UnboundedSender<SessionCommand>>,
    session: &SessionId,
    command: SessionCommand,
) {
    let Some(sender) = sessions.get(session) else {
        tracing::warn!(%session, "dropping a command for a session that was never opened");
        return;
    };
    if sender.send(command).is_err() {
        sessions.remove(session);
        tracing::warn!(%session, "dropping a command for a session whose connection ended");
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

    use futures::StreamExt;

    use super::*;

    /// How long a test waits for an event before failing instead of hanging.
    const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

    /// The next event, or `None` once the stream ended; fails the test after
    /// [`EVENT_TIMEOUT`].
    fn next_event(
        events: &mut event_channel::UnboundedReceiver<BackendEvent>,
    ) -> Option<BackendEvent> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime
            .block_on(async { tokio::time::timeout(EVENT_TIMEOUT, events.next()).await })
            .expect("no event within the timeout")
    }

    /// A config whose adapter cannot be spawned, so no test runs a real process.
    fn missing_agent() -> AcpConfig {
        AcpConfig {
            command: PathBuf::from("/nonexistent/partitur-test-agent"),
            ..AcpConfig::default()
        }
    }

    fn assert_unavailable(result: &Result<(), BackendError>) {
        assert!(
            matches!(result, Err(BackendError::Unavailable(_))),
            "expected Unavailable, got {result:?}"
        );
    }

    #[test]
    fn commands_after_shutdown_return_unavailable() {
        let (mut backend, _events) = AcpBackend::start(missing_agent()).unwrap();
        backend.stop();
        let session = SessionId::new();
        let cwd = Path::new("/work");
        assert_unavailable(&backend.new_session(RequestId(1), session.clone(), cwd));
        assert_unavailable(&backend.load_session(
            RequestId(2),
            session.clone(),
            &BackendSessionId::new("b"),
            cwd,
        ));
        assert_unavailable(
            &backend.prompt(session.clone(), vec![PromptContent::Text("hi".into())]),
        );
        assert_unavailable(&backend.cancel(session.clone()));
        assert_unavailable(&backend.set_config(
            session.clone(),
            &ConfigId::new("model"),
            &ConfigValueId::new("x"),
        ));
        assert_unavailable(&backend.respond_permission(session, PermissionId(1), None));
        assert_eq!(
            BackendError::Unavailable("the ACP runtime has stopped".into()).to_string(),
            "backend unavailable: the ACP runtime has stopped"
        );
    }

    #[test]
    fn commands_for_a_session_that_is_not_open_are_rejected() {
        let (backend, _events) = AcpBackend::start(missing_agent()).unwrap();
        let session = SessionId::new();
        let unknown = Err(BackendError::UnknownSession(session.clone()));
        assert_eq!(backend.prompt(session.clone(), Vec::new()), unknown);
        assert_eq!(backend.cancel(session.clone()), unknown);
        assert_eq!(
            backend.set_config(
                session.clone(),
                &ConfigId::new("model"),
                &ConfigValueId::new("x")
            ),
            unknown
        );
        assert_eq!(
            backend.respond_permission(session.clone(), PermissionId(1), None),
            unknown
        );
        backend.shutdown();
    }

    #[test]
    fn an_agent_that_cannot_start_fails_the_request() {
        let (backend, mut events) = AcpBackend::start(missing_agent()).unwrap();
        let created = SessionId::new();
        let loaded = SessionId::new();
        backend
            .new_session(RequestId(1), created.clone(), Path::new("/work"))
            .unwrap();
        backend
            .load_session(
                RequestId(2),
                loaded.clone(),
                &BackendSessionId::new("b-1"),
                Path::new("/work"),
            )
            .unwrap();
        let failed: Vec<_> = (0..2).map(|_| next_event(&mut events)).collect();
        // The spawn error's wording belongs to the OS; only its presence is pinned.
        for (expected_request, expected_session) in
            [(RequestId(1), created.clone()), (RequestId(2), loaded)]
        {
            assert!(
                failed.iter().any(|event| matches!(
                    event,
                    Some(BackendEvent::RequestFailed { request, session, error })
                        if *request == expected_request
                            && *session == expected_session
                            && !error.is_empty()
                )),
                "no RequestFailed for {expected_request} in {failed:?}"
            );
        }
        assert_eq!(
            backend.prompt(created.clone(), Vec::new()),
            Err(BackendError::UnknownSession(created))
        );
        backend.shutdown();
        assert_eq!(next_event(&mut events), None);
    }

    #[test]
    fn a_command_for_a_live_session_is_queued() {
        let (backend, mut events) = AcpBackend::start(missing_agent()).unwrap();
        let session = SessionId::new();
        // Stands in for a session whose connection is up; the runtime has no route for it
        // and drops the command, as it does for a connection that just ended.
        lock(&backend.live).insert(session.clone());
        assert_eq!(backend.cancel(session.clone()), Ok(()));
        assert_eq!(
            backend.respond_permission(session, PermissionId(7), None),
            Ok(())
        );
        backend.shutdown();
        assert_eq!(next_event(&mut events), None);
    }

    #[test]
    fn an_event_nobody_receives_is_dropped() {
        let (events, receiver) = event_channel::unbounded();
        drop(receiver);
        emit(
            &events,
            BackendEvent::Update {
                session: SessionId::new(),
                update: partitur_core::Update::Notice("unheard".into()),
            },
        );
        assert!(events.is_closed());
    }

    #[test]
    fn dropping_the_backend_shuts_the_runtime_down() {
        let (backend, mut events) = AcpBackend::start(missing_agent()).unwrap();
        let live = Arc::clone(&backend.live);
        drop(backend);
        // The runtime thread was joined, so it no longer holds the live-session set.
        assert_eq!(Arc::strong_count(&live), 1);
        assert_eq!(next_event(&mut events), None);
    }

    #[test]
    fn commands_reach_the_session_they_name() {
        let session = SessionId::new();
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let mut sessions = HashMap::from([(session.clone(), sender)]);
        route(&mut sessions, &session, SessionCommand::Cancel);
        assert!(matches!(receiver.try_recv(), Ok(SessionCommand::Cancel)));
        assert!(sessions.contains_key(&session));
    }

    #[test]
    fn commands_for_unknown_or_closed_sessions_are_dropped() {
        let open = SessionId::new();
        let closed = SessionId::new();
        let (open_sender, mut open_receiver) = mpsc::unbounded_channel();
        let (closed_sender, closed_receiver) = mpsc::unbounded_channel();
        drop(closed_receiver);
        let mut sessions =
            HashMap::from([(open.clone(), open_sender), (closed.clone(), closed_sender)]);
        route(&mut sessions, &SessionId::new(), SessionCommand::Cancel);
        route(&mut sessions, &closed, SessionCommand::Cancel);
        assert!(!sessions.contains_key(&closed));
        assert!(sessions.contains_key(&open));
        assert!(open_receiver.try_recv().is_err());
    }

    /// Runs `task` in a [`JoinSet`] and returns how it ended, as the runtime sees it.
    fn ended(task: impl Future<Output = ()> + Send + 'static) -> Result<(task::Id, ()), JoinError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut tasks = JoinSet::new();
            tasks.spawn(task);
            tasks.join_next_with_id().await.unwrap()
        })
    }

    fn owned_by(
        ended: &Result<(task::Id, ()), JoinError>,
        session: &SessionId,
    ) -> HashMap<task::Id, (SessionId, RequestId)> {
        let id = match ended {
            Ok((id, ())) => *id,
            Err(error) => error.id(),
        };
        HashMap::from([(id, (session.clone(), RequestId(3)))])
    }

    #[test]
    fn a_session_task_that_returned_is_only_forgotten() {
        let (events, mut inbox) = event_channel::unbounded();
        let session = SessionId::new();
        let returned = ended(async {});
        let mut owners = owned_by(&returned, &session);
        let (sender, _receiver) = mpsc::unbounded_channel();
        let mut sessions = HashMap::from([(session.clone(), sender)]);
        settle(
            returned,
            &mut owners,
            &mut sessions,
            &LiveSessions::default(),
            &events,
        );
        assert!(owners.is_empty());
        assert!(sessions.contains_key(&session));
        assert!(inbox.try_recv().is_err());
    }

    #[test]
    fn a_panicked_session_task_before_its_report_fails_the_request() {
        let (events, mut inbox) = event_channel::unbounded();
        let session = SessionId::new();
        let panicked = ended(async { panic!("session task panic") });
        let mut owners = owned_by(&panicked, &session);
        let (sender, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        let mut sessions = HashMap::from([(session.clone(), sender)]);
        settle(
            panicked,
            &mut owners,
            &mut sessions,
            &LiveSessions::default(),
            &events,
        );
        assert!(owners.is_empty());
        assert!(sessions.is_empty());
        assert!(matches!(
            inbox.try_recv(),
            Ok(BackendEvent::RequestFailed { request: RequestId(3), session: failed, error })
                if failed == session && error.starts_with("the session task failed: ")
        ));
    }

    #[test]
    fn a_panicked_session_task_after_its_report_closes_the_session() {
        let (events, mut inbox) = event_channel::unbounded();
        let session = SessionId::new();
        let live = LiveSessions::default();
        lock(&live).insert(session.clone());
        let panicked = ended(async { panic!("session task panic") });
        let mut owners = owned_by(&panicked, &session);
        // A newer connection of the session is still open and keeps its route.
        let (sender, _receiver) = mpsc::unbounded_channel();
        let mut sessions = HashMap::from([(session.clone(), sender)]);
        settle(panicked, &mut owners, &mut sessions, &live, &events);
        assert!(sessions.contains_key(&session));
        assert!(!lock(&live).contains(&session));
        assert!(matches!(
            inbox.try_recv(),
            Ok(BackendEvent::SessionClosed { session: closed, error })
                if closed == session && error.starts_with("the session task failed: ")
        ));
    }

    #[test]
    fn a_task_the_runtime_does_not_own_is_ignored() {
        let (events, mut inbox) = event_channel::unbounded();
        let panicked = ended(async { panic!("session task panic") });
        settle(
            panicked,
            &mut HashMap::new(),
            &mut HashMap::new(),
            &LiveSessions::default(),
            &events,
        );
        assert!(inbox.try_recv().is_err());
    }

    #[test]
    fn closing_all_sessions_cancels_their_turns_and_drops_their_routes() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let mut sessions = HashMap::from([(SessionId::new(), sender)]);
        close_all(&mut sessions);
        assert!(sessions.is_empty());
        assert!(matches!(receiver.try_recv(), Ok(SessionCommand::Cancel)));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn the_default_config_runs_the_claude_adapter_through_npx() {
        let config = AcpConfig::default();
        assert_eq!(config.command, PathBuf::from("npx"));
        assert_eq!(
            config.args,
            ["--yes", "@agentclientprotocol/claude-agent-acp"]
        );
        assert!(config.env.is_empty());
        assert_eq!(config.thinking_display, ThinkingDisplay::Summarized);
        assert_eq!(ThinkingDisplay::default(), ThinkingDisplay::Summarized);
        assert_eq!(ThinkingDisplay::Summarized.env_value(), "summarized");
        assert_eq!(ThinkingDisplay::Omitted.env_value(), "omitted");
    }

    #[test]
    fn start_errors_name_what_failed() {
        let error = AcpError::Spawn(io::Error::other("no threads"));
        assert_eq!(error.to_string(), "failed to spawn the ACP runtime thread");
        assert!(std::error::Error::source(&error).is_some());
        let error = AcpError::Runtime(io::Error::other("no runtime"));
        assert_eq!(error.to_string(), "failed to build the ACP runtime");
    }

    #[test]
    fn a_poisoned_lock_still_yields_its_data() {
        let mutex = Arc::new(Mutex::new(1));
        let poisoner = Arc::clone(&mutex);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poison the lock");
        })
        .join();
        assert!(mutex.is_poisoned());
        assert_eq!(*lock(&mutex), 1);
    }
}
