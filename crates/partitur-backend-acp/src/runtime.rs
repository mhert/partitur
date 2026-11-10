//! The [`AcpBackend`]: a tokio runtime on its own thread that owns one adapter process per
//! session, started from the config of the backend the session was opened on.
//!
//! [`Backend`] methods only queue a [`Command`] and return. The runtime thread routes each
//! command to the session it names, and every session's connection reports back through the
//! event channel handed out by [`AcpBackend::start`]. Only this crate names tokio: the event
//! receiver is a plain `futures` stream.
//!
//! The runtime also serves the MCP tool server of orchestrator sessions on a loopback port
//! bound at start; every orchestrator session is opened with its own URL on it.

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use futures::channel::mpsc as event_channel;
use partitur_core::{
    Backend, BackendError, BackendEvent, BackendId, BackendSessionId, ConfigId, ConfigValueId,
    PermissionId, PermissionOptionId, PromptContent, RequestId, SessionId, ToolRequestId, Update,
};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio::task::{self, JoinError, JoinSet};
use tokio::time::{Instant, sleep_until};

use crate::connection::{self, Open, SessionCommand, SessionStart};
use crate::mcp::{self, ANSWER_TIMEOUT};
use crate::permissions::PermissionIds;
use crate::tools::{PendingTools, ToolAnswer};

/// Names the runtime thread and its workers, so they are recognizable in a debugger.
const THREAD_NAME: &str = "partitur-acp";
/// Sessions spend their time waiting on pipes; two workers keep one busy session from
/// stalling the others.
const WORKER_THREADS: usize = 2;
/// How long shutdown waits for sessions to close their adapters before abandoning them.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// Where the tool server listens: a free port on the loopback interface, so only processes
/// on this machine reach it.
const TOOLS_ADDRESS: &str = "127.0.0.1:0";

/// The npm package of the Claude Code ACP adapter Partitur is built against, pinned to the
/// version it was tested with; bump it deliberately.
pub const ADAPTER_PACKAGE: &str = "@agentclientprotocol/claude-agent-acp@0.101.0";
/// The command the adapter package installs.
pub const ADAPTER_COMMAND: &str = "claude-agent-acp";

/// The sending half of the event channel every session reports through.
pub(crate) type Events = event_channel::UnboundedSender<BackendEvent>;

/// The number the runtime gives every connection it opens, so a session's newer connection
/// is told apart from the one it replaced.
type Generation = u64;

/// A session's current connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Current {
    /// The connection's number.
    generation: Generation,
    /// Whether the connection reported the session, so the core may command it.
    live: bool,
}

/// Every session's current connection and the next connection's number.
#[derive(Debug, Default)]
struct Registry {
    next: Generation,
    current: HashMap<SessionId, Current>,
}

/// Each session's current connection, shared by the runtime and every connection.
///
/// Opening a connection makes it its session's current one, which silences an older
/// connection of that session for good: whatever the older one still reports is dropped,
/// including how it ends. Forgetting a session silences its current connection the same way.
/// A session is live from the moment its current connection reports
/// it until that connection ends, so [`AcpBackend`] can reject commands for unknown sessions
/// synchronously.
#[derive(Debug, Clone, Default)]
pub(crate) struct Connections(Arc<Mutex<Registry>>);

impl Connections {
    /// Makes a new connection, not yet live, the current one of `session` and returns the
    /// reporter it sends its events through.
    pub(crate) fn claim(&self, session: SessionId, events: Events) -> Reporter {
        let mut registry = lock(&self.0);
        let generation = registry.next;
        registry.next += 1;
        registry.current.insert(
            session.clone(),
            Current {
                generation,
                live: false,
            },
        );
        Reporter {
            session,
            generation,
            connections: self.clone(),
            events,
        }
    }

    /// Forgets `session`'s current connection, so nothing it still reports reaches the core
    /// and the session is no longer live.
    pub(crate) fn forget(&self, session: &SessionId) {
        lock(&self.0).current.remove(session);
    }

    /// Whether `session`'s current connection has reported it and is still up.
    pub(crate) fn is_live(&self, session: &SessionId) -> bool {
        lock(&self.0)
            .current
            .get(session)
            .is_some_and(|current| current.live)
    }
}

/// Sends one connection's events to the core while it is its session's current connection;
/// once a newer connection of the session was opened it sends nothing.
///
/// Every event is sent with the registry locked, so none slips past a replacement.
#[derive(Debug, Clone)]
pub(crate) struct Reporter {
    session: SessionId,
    generation: Generation,
    connections: Connections,
    events: Events,
}

impl Reporter {
    /// The session this connection belongs to.
    pub(crate) fn session(&self) -> &SessionId {
        &self.session
    }

    /// This connection's entry in `registry`, while it is the session's current one.
    fn current<'a>(&self, registry: &'a mut Registry) -> Option<&'a mut Current> {
        registry
            .current
            .get_mut(&self.session)
            .filter(|current| current.generation == self.generation)
    }

    /// Reports `update` for the session.
    pub(crate) fn update(&self, update: Update) {
        let mut registry = lock(&self.connections.0);
        if self.current(&mut registry).is_some() {
            emit(
                &self.events,
                BackendEvent::Update {
                    session: self.session.clone(),
                    update,
                },
            );
        }
    }

    /// Makes the session live and sends `opened`, its created or loaded event; returns
    /// whether it did, which a replaced connection does not.
    pub(crate) fn go_live(&self, opened: BackendEvent) -> bool {
        let mut registry = lock(&self.connections.0);
        let Some(current) = self.current(&mut registry) else {
            return false;
        };
        current.live = true;
        emit(&self.events, opened);
        true
    }

    /// Forgets the session once this connection ended and sends the event `ending` builds
    /// from whether the session was live, if any. A replaced connection leaves its
    /// successor alone and sends nothing.
    pub(crate) fn retire(&self, ending: impl FnOnce(bool) -> Option<BackendEvent>) {
        let mut registry = lock(&self.connections.0);
        let Some(current) = self.current(&mut registry) else {
            return;
        };
        let live = current.live;
        registry.current.remove(&self.session);
        if let Some(event) = ending(live) {
            emit(&self.events, event);
        }
    }
}

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
    /// Runs the pinned Claude Code adapter [`ADAPTER_PACKAGE`] through `npx`, with
    /// summarized thinking.
    fn default() -> Self {
        Self {
            command: PathBuf::from("npx"),
            args: vec!["--yes".to_owned(), ADAPTER_PACKAGE.to_owned()],
            env: Vec::new(),
            thinking_display: ThinkingDisplay::Summarized,
        }
    }
}

/// Why the backend could not start.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AcpError {
    /// No backend was configured, so no session could ever be opened.
    #[error("no backend is configured")]
    NoBackends,
    /// The runtime thread could not be spawned.
    #[error("failed to spawn the ACP runtime thread")]
    Spawn(#[source] io::Error),
    /// The async runtime could not be built.
    #[error("failed to build the ACP runtime")]
    Runtime(#[source] io::Error),
    /// The orchestrator tool server could not listen on a loopback port.
    #[error("failed to start the orchestrator tool server")]
    Tools(#[source] io::Error),
}

/// A request from [`AcpBackend`] to its runtime thread.
pub(crate) enum Command {
    /// Starts an adapter process and creates or loads a session in it, replacing the
    /// session's older connection.
    Open {
        /// The backend whose adapter to start.
        backend: BackendId,
        /// The session to open.
        start: SessionStart,
        /// The URL of the session's tools, for an orchestrator session.
        tools_url: Option<String>,
    },
    /// A command for one open session.
    Session {
        /// The session the command is for.
        session: SessionId,
        /// What to do.
        command: SessionCommand,
    },
    /// Closes one session's connection, which ends its adapter, and silences it.
    Close {
        /// The session to close.
        session: SessionId,
    },
    /// Closes every session and stops the runtime.
    Shutdown,
}

/// The ACP [`Backend`]: one adapter process per session, driven from a runtime thread.
///
/// Each session runs the adapter configured for the backend it was opened on. Opening a
/// session that already has a connection replaces that connection: the old one is closed
/// and nothing it still reports reaches the core. Closing a session does the same without a
/// successor; a session still being opened closes once its adapter answered the open.
///
/// Dropping the backend shuts it down like [`AcpBackend::shutdown`].
#[derive(Debug)]
pub struct AcpBackend {
    commands: mpsc::UnboundedSender<Command>,
    connections: Connections,
    tools: PendingTools,
    /// The tool server's base URL, `http://127.0.0.1:{port}`.
    tools_base: String,
    thread: Option<JoinHandle<()>>,
}

impl AcpBackend {
    /// Starts the runtime thread serving the adapters in `backends`, keyed by the backend id
    /// sessions are opened on. No adapter process runs until a session is created or loaded;
    /// opening one on an id `backends` lacks fails that request with
    /// [`BackendEvent::RequestFailed`].
    ///
    /// Returns the backend and the receiver of every [`BackendEvent`] it reports. The
    /// receiver is a `futures` stream and needs no particular async runtime.
    ///
    /// ```no_run
    /// use std::collections::HashMap;
    /// use std::path::Path;
    ///
    /// use partitur_backend_acp::{AcpBackend, AcpConfig};
    /// use partitur_core::{Backend, BackendId, RequestId, SessionId};
    ///
    /// let claude = BackendId::try_from("claude")?;
    /// let backends = HashMap::from([(claude.clone(), AcpConfig::default())]);
    /// let (backend, events) = AcpBackend::start(backends)?;
    /// backend.new_session(RequestId(1), SessionId::new(), &claude, Path::new("/work"), false)?;
    /// // `events` yields `SessionCreated`, then the session's updates.
    /// # drop(events);
    /// backend.shutdown();
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`AcpError::NoBackends`] when `backends` is empty, [`AcpError::Runtime`] when
    /// the async runtime cannot be built, [`AcpError::Tools`] when the tool server cannot
    /// listen and [`AcpError::Spawn`] when the runtime's thread cannot be spawned.
    pub fn start<S: BuildHasher>(
        backends: HashMap<BackendId, AcpConfig, S>,
    ) -> Result<(Self, event_channel::UnboundedReceiver<BackendEvent>), AcpError> {
        if backends.is_empty() {
            return Err(AcpError::NoBackends);
        }
        let backends = backends
            .into_iter()
            .map(|(id, config)| (id, Arc::new(config)))
            .collect();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKER_THREADS)
            .thread_name(THREAD_NAME)
            .enable_all()
            .build()
            .map_err(AcpError::Runtime)?;
        let (listener, tools_base) = tool_listener(&runtime).map_err(AcpError::Tools)?;
        let (commands, command_receiver) = mpsc::unbounded_channel();
        let (event_sender, event_receiver) = event_channel::unbounded();
        let connections = Connections::default();
        let tools = PendingTools::default();
        let thread = std::thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn({
                let connections = connections.clone();
                let tools = tools.clone();
                move || {
                    runtime.block_on(run(
                        command_receiver,
                        backends,
                        event_sender,
                        connections,
                        tools,
                        listener,
                    ));
                }
            })
            .map_err(AcpError::Spawn)?;
        Ok((
            Self {
                commands,
                connections,
                tools,
                tools_base,
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

    /// Queues opening `start` on `backend`, with the URL of its tools for an orchestrator.
    fn open(&self, backend: &BackendId, start: SessionStart) -> Result<(), BackendError> {
        let tools_url = start
            .orchestrator
            .then(|| mcp::session_url(&self.tools_base, &start.session));
        self.send(Command::Open {
            backend: backend.clone(),
            start,
            tools_url,
        })
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
        if !self.connections.is_live(&session) {
            return Err(BackendError::UnknownSession(session));
        }
        self.send(Command::Session { session, command })
    }
}

/// Binds the tool server's listener on [`TOOLS_ADDRESS`], registered with `runtime`, which
/// serves it, and returns it with its base URL.
///
/// # Errors
///
/// Returns the I/O error when no port can be bound, the listener cannot be made
/// non-blocking or `runtime` cannot register it.
fn tool_listener(runtime: &Runtime) -> io::Result<(TcpListener, String)> {
    let listener = std::net::TcpListener::bind(TOOLS_ADDRESS)?;
    listener.set_nonblocking(true)?;
    let base = format!("http://{}", listener.local_addr()?);
    let _runtime = runtime.enter();
    Ok((TcpListener::from_std(listener)?, base))
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
        backend: &BackendId,
        cwd: &Path,
        orchestrator: bool,
    ) -> Result<(), BackendError> {
        self.open(
            backend,
            SessionStart {
                request,
                session,
                open: Open::New,
                cwd: cwd.to_path_buf(),
                orchestrator,
            },
        )
    }

    fn load_session(
        &self,
        request: RequestId,
        session: SessionId,
        backend: &BackendId,
        backend_session: &BackendSessionId,
        cwd: &Path,
        orchestrator: bool,
    ) -> Result<(), BackendError> {
        self.open(
            backend,
            SessionStart {
                request,
                session,
                open: Open::Load(backend_session.clone()),
                cwd: cwd.to_path_buf(),
                orchestrator,
            },
        )
    }

    fn prompt(&self, session: SessionId, content: Vec<PromptContent>) -> Result<(), BackendError> {
        self.send_to(session, SessionCommand::Prompt(content))
    }

    fn cancel(&self, session: SessionId) -> Result<(), BackendError> {
        self.send_to(session, SessionCommand::Cancel)
    }

    /// Queues the close for any session, live or not: one still being opened, or one the
    /// runtime does not know, is closed or ignored in the order the commands were queued.
    fn close_session(&self, session: SessionId) -> Result<(), BackendError> {
        self.send(Command::Close { session })
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

    /// Hands the answer to the parked call at once; no runtime round trip is needed.
    fn answer_tool(&self, call: ToolRequestId, result: ToolAnswer) -> Result<(), BackendError> {
        self.tools.answer(call, result)
    }
}

/// The way to a session's current connection.
#[derive(Debug)]
struct Route {
    /// The connection's number.
    generation: Generation,
    /// Hands the connection its commands; dropping it closes the connection.
    sender: mpsc::UnboundedSender<SessionCommand>,
}

/// What the runtime remembers about a running session task.
#[derive(Debug)]
struct Owner {
    /// The create or load request the task answers.
    request: RequestId,
    /// The task's connection.
    reporter: Reporter,
}

/// The runtime's main loop: serves the tool server on `listener`, spawns a task per opened
/// session, routes commands to it and settles tasks as they end. After [`Command::Shutdown`]
/// (or once the backend is gone) it closes every session, waits up to [`SHUTDOWN_GRACE`] for
/// their tasks and stops the tool server. Tool calls are reported through `events` and
/// parked in `tools`, where the backend answers them.
async fn run(
    mut commands: mpsc::UnboundedReceiver<Command>,
    backends: HashMap<BackendId, Arc<AcpConfig>>,
    events: Events,
    connections: Connections,
    tools: PendingTools,
    listener: TcpListener,
) {
    let tool_server = task::spawn(mcp::serve(
        listener,
        mcp::router(events.clone(), tools, ANSWER_TIMEOUT),
    ));
    let mut routes = HashMap::new();
    let mut tasks = JoinSet::new();
    let mut owners = HashMap::new();
    // One counter for every connection, so a reconnected session never repeats an id.
    let permission_ids = PermissionIds::default();
    // Set once shutdown began: the moment the remaining sessions are abandoned.
    let mut deadline: Option<Instant> = None;
    loop {
        if deadline.is_some() && tasks.is_empty() {
            break;
        }
        tokio::select! {
            Some(ended) = tasks.join_next_with_id() => {
                settle(ended, &mut owners, &mut routes);
            }
            command = commands.recv(), if deadline.is_none() => match command {
                Some(Command::Open { backend, start, tools_url }) => {
                    if let Some(config) = backends.get(&backend) {
                        // Claiming the session silences its older connection, and replacing
                        // that one's route drops its sender, which closes it.
                        let reporter = connections.claim(start.session.clone(), events.clone());
                        let (sender, receiver) = mpsc::unbounded_channel();
                        let route = Route { generation: reporter.generation, sender };
                        routes.insert(start.session.clone(), route);
                        let request = start.request;
                        let task = tasks.spawn(connection::run_session(
                            Arc::clone(config),
                            start,
                            receiver,
                            reporter.clone(),
                            Arc::clone(&permission_ids),
                            tools_url,
                        ));
                        owners.insert(task.id(), Owner { request, reporter });
                    } else {
                        emit(&events, not_configured(&backend, start));
                    }
                }
                Some(Command::Session { session, command }) => {
                    route(&mut routes, &session, command);
                }
                Some(Command::Close { session }) => {
                    close(&mut routes, &connections, &session);
                }
                Some(Command::Shutdown) | None => {
                    close_all(&mut routes);
                    deadline = Some(Instant::now() + SHUTDOWN_GRACE);
                }
            },
            () = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                tracing::warn!("ACP sessions did not close in time, killing their processes");
                break;
            }
        }
    }
    tool_server.abort();
}

/// The event failing `start`, which named `backend`, a backend nobody configured.
fn not_configured(backend: &BackendId, start: SessionStart) -> BackendEvent {
    BackendEvent::RequestFailed {
        request: start.request,
        session: start.session,
        error: format!("backend {backend} is not configured"),
    }
}

/// Cancels every session's turn and drops its route, which ends its command loop and closes
/// its adapter.
fn close_all(routes: &mut HashMap<SessionId, Route>) {
    for (_, route) in routes.drain() {
        // A session whose connection already ended has nothing to cancel.
        let _ = route.sender.send(SessionCommand::Cancel);
    }
}

/// Closes `session`'s current connection: forgets it first, so nothing the connection still
/// reports reaches the core, then cancels its turn and drops its route, which ends its command
/// loop and closes its adapter. A session without a connection is only forgotten.
fn close(routes: &mut HashMap<SessionId, Route>, connections: &Connections, session: &SessionId) {
    connections.forget(session);
    if let Some(route) = routes.remove(session) {
        // A session whose connection already ended has nothing to cancel.
        let _ = route.sender.send(SessionCommand::Cancel);
    }
}

/// Settles a session task that ended. A task that returned has already reported its end;
/// one that panicked or was aborted is logged, forgotten, and reported to the core like a
/// connection that broke: a closed session once it was reported, else a failed request. A
/// task whose connection was replaced reports nothing and leaves the newer one alone.
fn settle(
    ended: Result<(task::Id, ()), JoinError>,
    owners: &mut HashMap<task::Id, Owner>,
    routes: &mut HashMap<SessionId, Route>,
) {
    let (id, failure) = match ended {
        Ok((id, ())) => (id, None),
        Err(error) => (error.id(), Some(error)),
    };
    let Some(Owner { request, reporter }) = owners.remove(&id) else {
        return;
    };
    let Some(failure) = failure else { return };
    let session = reporter.session();
    tracing::error!(%session, error = %failure, "an ACP session task ended abnormally");
    if routes
        .get(session)
        .is_some_and(|route| route.generation == reporter.generation)
    {
        routes.remove(session);
    }
    let error = format!("the session task failed: {failure}");
    reporter.retire(|live| {
        let session = session.clone();
        Some(if live {
            BackendEvent::SessionClosed { session, error }
        } else {
            BackendEvent::RequestFailed {
                request,
                session,
                error,
            }
        })
    });
}

/// Hands `command` to `session`'s connection, forgetting sessions whose connection ended.
fn route(routes: &mut HashMap<SessionId, Route>, session: &SessionId, command: SessionCommand) {
    let Some(route) = routes.get(session) else {
        tracing::warn!(%session, "dropping a command for a session that was never opened");
        return;
    };
    if route.sender.send(command).is_err() {
        routes.remove(session);
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

    fn claude() -> BackendId {
        BackendId::try_from("claude").unwrap()
    }

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

    /// A backend map whose only adapter, under [`claude`], cannot be spawned, so no test
    /// runs a real process.
    fn missing_agent() -> HashMap<BackendId, AcpConfig> {
        let config = AcpConfig {
            command: PathBuf::from("/nonexistent/partitur-test-agent"),
            ..AcpConfig::default()
        };
        HashMap::from([(claude(), config)])
    }

    /// The registry's entry for `session`, if it has one.
    fn current(connections: &Connections, session: &SessionId) -> Option<Current> {
        lock(&connections.0).current.get(session).copied()
    }

    /// Opens a connection of `session` in `connections` and makes it live, as reporting the
    /// session does.
    fn live(connections: &Connections, session: &SessionId, events: &Events) -> Reporter {
        let reporter = connections.claim(session.clone(), events.clone());
        assert!(reporter.go_live(BackendEvent::Update {
            session: session.clone(),
            update: Update::Notice("live".into()),
        }));
        reporter
    }

    fn notice(session: &SessionId, text: &str) -> BackendEvent {
        BackendEvent::Update {
            session: session.clone(),
            update: Update::Notice(text.into()),
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
        assert_unavailable(&backend.new_session(
            RequestId(1),
            session.clone(),
            &claude(),
            cwd,
            false,
        ));
        assert_unavailable(&backend.load_session(
            RequestId(2),
            session.clone(),
            &claude(),
            &BackendSessionId::new("b"),
            cwd,
            false,
        ));
        assert_unavailable(
            &backend.prompt(session.clone(), vec![PromptContent::Text("hi".into())]),
        );
        assert_unavailable(&backend.cancel(session.clone()));
        assert_unavailable(&backend.close_session(session.clone()));
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
        assert_eq!(
            backend.answer_tool(ToolRequestId(1), Ok("result".into())),
            Err(BackendError::UnknownToolCall(ToolRequestId(1)))
        );
        backend.shutdown();
    }

    #[test]
    fn an_agent_that_cannot_start_fails_the_request() {
        let (backend, mut events) = AcpBackend::start(missing_agent()).unwrap();
        let created = SessionId::new();
        let loaded = SessionId::new();
        backend
            .new_session(
                RequestId(1),
                created.clone(),
                &claude(),
                Path::new("/work"),
                false,
            )
            .unwrap();
        backend
            .load_session(
                RequestId(2),
                loaded.clone(),
                &claude(),
                &BackendSessionId::new("b-1"),
                Path::new("/work"),
                false,
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
        let (unheard, _inbox) = event_channel::unbounded();
        live(&backend.connections, &session, &unheard);
        assert_eq!(backend.cancel(session.clone()), Ok(()));
        assert_eq!(
            backend.respond_permission(session, PermissionId(7), None),
            Ok(())
        );
        backend.shutdown();
        assert_eq!(next_event(&mut events), None);
    }

    #[test]
    fn closing_a_session_the_backend_never_opened_is_no_error() {
        let (backend, mut events) = AcpBackend::start(missing_agent()).unwrap();
        assert_eq!(backend.close_session(SessionId::new()), Ok(()));
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
        let registry = Arc::clone(&backend.connections.0);
        drop(backend);
        // The runtime thread was joined, so it no longer holds the registry.
        assert_eq!(Arc::strong_count(&registry), 1);
        assert_eq!(next_event(&mut events), None);
    }

    #[test]
    fn start_with_no_backends_is_an_error() {
        let started = AcpBackend::start(HashMap::new());
        assert!(matches!(started, Err(AcpError::NoBackends)));
        assert_eq!(AcpError::NoBackends.to_string(), "no backend is configured");
    }

    #[test]
    fn opening_a_session_on_an_unknown_backend_fails_its_request() {
        let session = SessionId::new();
        let start = SessionStart {
            request: RequestId(4),
            session: session.clone(),
            open: Open::New,
            cwd: PathBuf::from("/work"),
            orchestrator: false,
        };
        assert_eq!(
            not_configured(&BackendId::try_from("gone").unwrap(), start),
            BackendEvent::RequestFailed {
                request: RequestId(4),
                session,
                error: "backend gone is not configured".into(),
            }
        );
    }

    #[test]
    fn a_claimed_session_is_live_only_once_reported() {
        let (events, mut inbox) = event_channel::unbounded();
        let connections = Connections::default();
        let session = SessionId::new();
        let reporter = connections.claim(session.clone(), events);
        assert!(!connections.is_live(&session));
        assert!(reporter.go_live(notice(&session, "created")));
        assert!(connections.is_live(&session));
        assert!(!connections.is_live(&SessionId::new()));
        reporter.update(Update::Notice("update".into()));
        assert_eq!(inbox.try_recv().ok(), Some(notice(&session, "created")));
        assert_eq!(inbox.try_recv().ok(), Some(notice(&session, "update")));
    }

    #[test]
    fn every_claim_gets_its_own_generation() {
        let (events, _inbox) = event_channel::unbounded();
        let connections = Connections::default();
        let first = connections.claim(SessionId::new(), events.clone());
        let second = connections.claim(SessionId::new(), events);
        assert_eq!((first.generation, second.generation), (0, 1));
    }

    #[test]
    fn a_replaced_connection_reports_nothing_and_leaves_its_successor_alone() {
        let (events, mut inbox) = event_channel::unbounded();
        let connections = Connections::default();
        let session = SessionId::new();
        let old = live(&connections, &session, &events);
        let new = connections.claim(session.clone(), events);
        assert_eq!(inbox.try_recv().ok(), Some(notice(&session, "live")));
        // The newer connection is not live until it reports the session.
        assert!(!connections.is_live(&session));

        old.update(Update::Notice("late".into()));
        assert!(!old.go_live(notice(&session, "reopened")));
        old.retire(|_| Some(notice(&session, "ended")));
        assert!(inbox.try_recv().is_err());
        assert_eq!(
            current(&connections, &session),
            Some(Current {
                generation: new.generation,
                live: false
            })
        );
    }

    #[test]
    fn retiring_the_current_connection_forgets_the_session() {
        let (events, mut inbox) = event_channel::unbounded();
        let connections = Connections::default();
        let session = SessionId::new();
        let reporter = live(&connections, &session, &events);
        let _ = inbox.try_recv();
        reporter.retire(|live| Some(notice(&session, &format!("live: {live}"))));
        assert_eq!(current(&connections, &session), None);
        assert_eq!(inbox.try_recv().ok(), Some(notice(&session, "live: true")));

        // A connection that never reported its session ends without having been live, and
        // an ending that needs no event sends none.
        let unreported = connections.claim(session.clone(), events);
        unreported.retire(|live| Some(notice(&session, &format!("live: {live}"))));
        assert_eq!(inbox.try_recv().ok(), Some(notice(&session, "live: false")));
        let closed = connections.claim(session.clone(), unreported.events.clone());
        closed.retire(|_| None);
        assert_eq!(current(&connections, &session), None);
        assert!(inbox.try_recv().is_err());
    }

    /// A route to a connection numbered `generation`, and the receiving end of its commands.
    fn route_to(generation: Generation) -> (Route, mpsc::UnboundedReceiver<SessionCommand>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Route { generation, sender }, receiver)
    }

    #[test]
    fn commands_reach_the_session_they_name() {
        let session = SessionId::new();
        let (to_session, mut receiver) = route_to(0);
        let mut routes = HashMap::from([(session.clone(), to_session)]);
        route(&mut routes, &session, SessionCommand::Cancel);
        assert!(matches!(receiver.try_recv(), Ok(SessionCommand::Cancel)));
        assert!(routes.contains_key(&session));
    }

    #[test]
    fn commands_for_unknown_or_closed_sessions_are_dropped() {
        let open = SessionId::new();
        let closed = SessionId::new();
        let (open_route, mut open_receiver) = route_to(0);
        let (closed_route, closed_receiver) = route_to(1);
        drop(closed_receiver);
        let mut routes =
            HashMap::from([(open.clone(), open_route), (closed.clone(), closed_route)]);
        route(&mut routes, &SessionId::new(), SessionCommand::Cancel);
        route(&mut routes, &closed, SessionCommand::Cancel);
        assert!(!routes.contains_key(&closed));
        assert!(routes.contains_key(&open));
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
        reporter: Reporter,
    ) -> HashMap<task::Id, Owner> {
        let id = match ended {
            Ok((id, ())) => *id,
            Err(error) => error.id(),
        };
        let owner = Owner {
            request: RequestId(3),
            reporter,
        };
        HashMap::from([(id, owner)])
    }

    #[test]
    fn a_session_task_that_returned_is_only_forgotten() {
        let (events, mut inbox) = event_channel::unbounded();
        let connections = Connections::default();
        let session = SessionId::new();
        let reporter = live(&connections, &session, &events);
        let _ = inbox.try_recv();
        let returned = ended(async {});
        let mut owners = owned_by(&returned, reporter);
        let (to_session, _receiver) = route_to(0);
        let mut routes = HashMap::from([(session.clone(), to_session)]);
        settle(returned, &mut owners, &mut routes);
        assert!(owners.is_empty());
        assert!(routes.contains_key(&session));
        assert!(connections.is_live(&session));
        assert!(inbox.try_recv().is_err());
    }

    #[test]
    fn a_panicked_session_task_before_its_report_fails_the_request() {
        let (events, mut inbox) = event_channel::unbounded();
        let connections = Connections::default();
        let session = SessionId::new();
        let reporter = connections.claim(session.clone(), events);
        let panicked = ended(async { panic!("session task panic") });
        let (to_session, _receiver) = route_to(reporter.generation);
        let mut owners = owned_by(&panicked, reporter);
        let mut routes = HashMap::from([(session.clone(), to_session)]);
        settle(panicked, &mut owners, &mut routes);
        assert!(owners.is_empty());
        assert!(routes.is_empty());
        assert_eq!(current(&connections, &session), None);
        assert!(matches!(
            inbox.try_recv(),
            Ok(BackendEvent::RequestFailed { request: RequestId(3), session: failed, error })
                if failed == session && error.starts_with("the session task failed: ")
        ));
    }

    #[test]
    fn a_panicked_session_task_after_its_report_closes_the_session() {
        let (events, mut inbox) = event_channel::unbounded();
        let connections = Connections::default();
        let session = SessionId::new();
        let reporter = live(&connections, &session, &events);
        let _ = inbox.try_recv();
        let panicked = ended(async { panic!("session task panic") });
        let (to_session, _receiver) = route_to(reporter.generation);
        let mut owners = owned_by(&panicked, reporter);
        let mut routes = HashMap::from([(session.clone(), to_session)]);
        settle(panicked, &mut owners, &mut routes);
        assert!(routes.is_empty());
        assert!(!connections.is_live(&session));
        assert!(matches!(
            inbox.try_recv(),
            Ok(BackendEvent::SessionClosed { session: closed, error })
                if closed == session && error.starts_with("the session task failed: ")
        ));
    }

    #[test]
    fn a_panicked_task_of_a_replaced_connection_leaves_the_newer_one_alone() {
        let (events, mut inbox) = event_channel::unbounded();
        let connections = Connections::default();
        let session = SessionId::new();
        let old = live(&connections, &session, &events);
        let new = live(&connections, &session, &events);
        let _ = (inbox.try_recv(), inbox.try_recv());
        let panicked = ended(async { panic!("session task panic") });
        let mut owners = owned_by(&panicked, old);
        let (to_new, _receiver) = route_to(new.generation);
        let mut routes = HashMap::from([(session.clone(), to_new)]);
        settle(panicked, &mut owners, &mut routes);
        assert!(owners.is_empty());
        // The newer connection keeps its route and its live entry, and nothing is reported.
        assert!(routes.contains_key(&session));
        assert!(connections.is_live(&session));
        assert!(inbox.try_recv().is_err());
    }

    #[test]
    fn a_task_the_runtime_does_not_own_is_ignored() {
        let panicked = ended(async { panic!("session task panic") });
        let mut routes = HashMap::from([(SessionId::new(), route_to(0).0)]);
        settle(panicked, &mut HashMap::new(), &mut routes);
        assert_eq!(routes.len(), 1);
    }

    #[test]
    fn closing_all_sessions_cancels_their_turns_and_drops_their_routes() {
        let (to_session, mut receiver) = route_to(0);
        let mut routes = HashMap::from([(SessionId::new(), to_session)]);
        close_all(&mut routes);
        assert!(routes.is_empty());
        assert!(matches!(receiver.try_recv(), Ok(SessionCommand::Cancel)));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn closing_a_session_silences_it_cancels_its_turn_and_drops_its_route() {
        let (events, mut inbox) = event_channel::unbounded();
        let connections = Connections::default();
        let session = SessionId::new();
        let other = SessionId::new();
        let reporter = live(&connections, &session, &events);
        live(&connections, &other, &events);
        let _ = (inbox.try_recv(), inbox.try_recv());
        let (to_session, mut receiver) = route_to(reporter.generation);
        let (to_other, _other_receiver) = route_to(1);
        let mut routes = HashMap::from([(session.clone(), to_session), (other.clone(), to_other)]);

        close(&mut routes, &connections, &session);

        assert!(!routes.contains_key(&session));
        assert!(routes.contains_key(&other));
        assert!(matches!(receiver.try_recv(), Ok(SessionCommand::Cancel)));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ));
        assert_eq!(current(&connections, &session), None);
        assert!(connections.is_live(&other));
        reporter.update(Update::Notice("late".into()));
        reporter.retire(|_| Some(notice(&session, "ended")));
        assert!(inbox.try_recv().is_err());
    }

    #[test]
    fn closing_a_session_without_a_route_only_forgets_it() {
        let (events, _inbox) = event_channel::unbounded();
        let connections = Connections::default();
        let session = SessionId::new();
        let opening = connections.claim(session.clone(), events);
        let (to_other, _receiver) = route_to(5);
        let mut routes = HashMap::from([(SessionId::new(), to_other)]);

        close(&mut routes, &connections, &session);

        assert_eq!(routes.len(), 1);
        assert_eq!(current(&connections, &session), None);
        assert!(!opening.go_live(notice(&session, "created")));
    }

    #[test]
    fn the_default_config_runs_the_pinned_claude_adapter_through_npx() {
        let config = AcpConfig::default();
        assert_eq!(config.command, PathBuf::from("npx"));
        assert_eq!(
            config.args,
            ["--yes", "@agentclientprotocol/claude-agent-acp@0.101.0"]
        );
        assert_eq!(ADAPTER_COMMAND, "claude-agent-acp");
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
        let error = AcpError::Tools(io::Error::other("no port"));
        assert_eq!(
            error.to_string(),
            "failed to start the orchestrator tool server"
        );
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn the_tool_server_listens_on_the_loopback_interface() {
        let (backend, _events) = AcpBackend::start(missing_agent()).unwrap();
        let port = backend
            .tools_base
            .strip_prefix("http://127.0.0.1:")
            .unwrap_or_else(|| panic!("not a loopback URL: {}", backend.tools_base));
        assert!(std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok());
        backend.shutdown();
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
