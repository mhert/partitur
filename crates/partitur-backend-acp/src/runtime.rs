//! The [`AcpBackend`]: a tokio runtime on its own thread that owns one adapter process per
//! session, started from the config of the backend the session was opened on.
//!
//! [`Backend`] methods only queue a [`Command`] and return. The runtime thread routes each
//! command to the session it names, and every session's connection reports back through the
//! event channel handed out by [`AcpBackend::start`]. Only this crate names tokio: the event
//! receiver is a plain `futures` stream.
//!
//! The backends may also be resolved after the runtime started, with
//! [`AcpBackend::start_deferred`]: its resolver runs on a thread of its own, and until it
//! delivers, the runtime holds every open in the order it arrived while it still serves closes
//! and shutdown at once. A held open whose session is closed is dropped, shutdown drops every
//! held open without waiting for the resolver, and once the backends are known the held opens
//! run in order. A held open's session is not live, so the backend refuses its prompts and
//! other session commands as it does for any session that is not open; one that still reaches
//! the runtime is dropped, as is any command for a session that was never opened. A backend
//! that failed to resolve fails each open on it with the reason it failed for, and a resolver
//! that panics leaves every backend unconfigured.
//!
//! Every session is opened with the URL of its tools, which the backend looks up with the
//! function handed to [`AcpBackend::start`]; the backend serves no tools itself.
//!
//! Once the backends are known, and before any session's adapter starts, the runtime writes
//! Partitur's tool instructions into each backend's instructions folder, once per folder, with
//! [`write_instructions`]. A folder that cannot be written is logged, and every session opened
//! on a backend using it is told so with a notice once it is reported.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs;
use std::hash::BuildHasher;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use futures::channel::mpsc as event_channel;
use partitur_core::{
    AgentUpdate, Backend, BackendError, BackendEvent, BackendId, BackendSessionId, ConfigId,
    ConfigValueId, PermissionId, PermissionOptionId, PromptContent, RequestId, SessionId, ToolRole,
};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::task::{self, JoinError, JoinSet};
use tokio::time::{Instant, sleep_until};

use crate::connection::{self, Open, SessionCommand, SessionStart};
use crate::permissions::PermissionIds;

/// Names the runtime thread and its workers, so they are recognizable in a debugger.
const THREAD_NAME: &str = "partitur-acp";
/// Names the thread that resolves the backends of [`AcpBackend::start_deferred`].
const RESOLVE_THREAD_NAME: &str = "acp-resolve";
/// Sessions spend their time waiting on pipes; two workers keep one busy session from
/// stalling the others.
const WORKER_THREADS: usize = 2;
/// How long shutdown waits for sessions to close their adapters before abandoning them.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// The npm package of the Claude Code ACP adapter Partitur is built against, pinned to the
/// version it was tested with; bump it deliberately.
pub const ADAPTER_PACKAGE: &str = "@agentclientprotocol/claude-agent-acp@0.108.0";
/// The command the adapter package installs.
pub const ADAPTER_COMMAND: &str = "claude-agent-acp";

/// The file in an instructions folder that holds Partitur's tool instructions; agents such as
/// GitHub Copilot CLI inline every plain `*.instructions.md` of the folder.
pub const INSTRUCTIONS_FILE: &str = "partitur.instructions.md";

/// The sending half of the event channel every session reports through.
pub(crate) type Events = event_channel::UnboundedSender<BackendEvent>;

/// How to start each backend's adapter, or why it cannot be started, by backend id.
pub type Resolution = HashMap<BackendId, Result<AcpConfig, String>>;

/// The resolved backends as the runtime keeps them.
type Backends = HashMap<BackendId, Result<Arc<AcpConfig>, String>>;

/// Looks up the URL of a session's tools.
type ToolUrl = Box<dyn Fn(&SessionId, ToolRole) -> String + Send + Sync>;

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
    pub(crate) fn update(&self, update: AgentUpdate) {
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
    /// The folder Partitur's tool instructions are written to, named to the adapter in
    /// `COPILOT_CUSTOM_INSTRUCTIONS_DIRS` for agents that read them from there; `None` names
    /// no folder.
    pub instructions_dir: Option<PathBuf>,
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
            instructions_dir: None,
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
    /// Partitur's tool instructions could not be written to their folder.
    #[error("failed to write Partitur's instructions")]
    Instructions(#[source] io::Error),
}

/// Writes Partitur's tool instructions to [`INSTRUCTIONS_FILE`] in `dir`, creating `dir` first,
/// and returns the file's path. A file that already holds them is left alone, so its
/// modification time tells when they last changed.
///
/// # Errors
///
/// Returns [`AcpError::Instructions`] when `dir` cannot be created or the file cannot be
/// written.
pub fn write_instructions(dir: &Path) -> Result<PathBuf, AcpError> {
    let file = dir.join(INSTRUCTIONS_FILE);
    let instructions = partitur_core::tool_instructions();
    fs::create_dir_all(dir).map_err(AcpError::Instructions)?;
    if fs::read(&file).ok().as_deref() != Some(instructions.as_bytes()) {
        fs::write(&file, instructions).map_err(AcpError::Instructions)?;
    }
    Ok(file)
}

/// Writes the instructions into every folder the configs in `backends` name, once each, and
/// returns the folders that could not be written, each logged.
fn write_every_instructions(backends: &Backends) -> HashSet<PathBuf> {
    let folders: HashSet<&PathBuf> = backends
        .values()
        .filter_map(|config| config.as_ref().ok()?.instructions_dir.as_ref())
        .collect();
    folders
        .into_iter()
        .filter(|dir| {
            write_instructions(dir)
                .inspect_err(|error| {
                    let shown = dir.display();
                    let source = std::error::Error::source(error);
                    tracing::error!(
                        dir = %shown,
                        error = %error,
                        source = ?source,
                        "failed to write Partitur's instructions"
                    );
                })
                .is_err()
        })
        .cloned()
        .collect()
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
        /// The URL of the session's tools.
        tools_url: String,
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
pub struct AcpBackend {
    commands: mpsc::UnboundedSender<Command>,
    connections: Connections,
    /// The URL of a session's tools, by session and tool role.
    tool_url: ToolUrl,
    thread: Option<JoinHandle<()>>,
}

/// Shows the connections; the tool URL lookup is a function and is left out.
impl fmt::Debug for AcpBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AcpBackend")
            .field("connections", &self.connections)
            .finish_non_exhaustive()
    }
}

impl AcpBackend {
    /// Starts the runtime thread serving the adapters in `backends`, keyed by the backend id
    /// sessions are opened on. No adapter process runs until a session is created or loaded;
    /// opening one on an id `backends` lacks fails that request with
    /// [`BackendEvent::RequestFailed`]. Every session is opened with
    /// `tool_url(session, role)` as the URL of its tools.
    ///
    /// Returns the backend and the receiver of every [`BackendEvent`] it reports. The
    /// receiver is a `futures` stream and needs no particular async runtime.
    ///
    /// ```no_run
    /// use std::collections::HashMap;
    /// use std::path::Path;
    ///
    /// use partitur_backend_acp::{AcpBackend, AcpConfig};
    /// use partitur_core::{Backend, BackendId, RequestId, SessionId, ToolRole};
    ///
    /// let claude = BackendId::try_from("claude")?;
    /// let backends = HashMap::from([(claude.clone(), AcpConfig::default())]);
    /// let tool_url = |session: &SessionId, role: ToolRole| {
    ///     let suffix = match role {
    ///         ToolRole::Plain => "",
    ///         ToolRole::Orchestrator => "/orchestrator",
    ///         ToolRole::Child => "/child",
    ///     };
    ///     format!("http://127.0.0.1:4000/{session}{suffix}")
    /// };
    /// let (backend, events) = AcpBackend::start(backends, tool_url)?;
    /// backend.new_session(
    ///     RequestId(1),
    ///     SessionId::new(),
    ///     &claude,
    ///     Path::new("/work"),
    ///     ToolRole::Plain,
    /// )?;
    /// // `events` yields `SessionCreated`, then the session's updates.
    /// # drop(events);
    /// backend.shutdown();
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`AcpError::NoBackends`] when `backends` is empty, [`AcpError::Runtime`] when
    /// the async runtime cannot be built and [`AcpError::Spawn`] when the runtime's thread
    /// cannot be spawned.
    pub fn start<S: BuildHasher>(
        backends: HashMap<BackendId, AcpConfig, S>,
        tool_url: impl Fn(&SessionId, ToolRole) -> String + Send + Sync + 'static,
    ) -> Result<(Self, event_channel::UnboundedReceiver<BackendEvent>), AcpError> {
        if backends.is_empty() {
            return Err(AcpError::NoBackends);
        }
        let resolution: Resolution = backends
            .into_iter()
            .map(|(id, config)| (id, Ok(config)))
            .collect();
        // Known before the runtime sees its first command, so no open is ever held.
        let (resolved, receiver) = oneshot::channel();
        let _ = resolved.send(resolution);
        Self::launch(receiver, tool_url)
    }

    /// Starts the runtime thread like [`AcpBackend::start`], but with backends that are only
    /// known once `resolve` returns. `resolve` runs on a thread of its own, so it may block,
    /// and the runtime does not wait for it to shut down. Until it returns, every create or
    /// load is held and runs, in the order it was queued, once the backends are known; a held
    /// one whose session is closed meanwhile is dropped. Opening a session on a backend that
    /// resolved to `Err(reason)` fails that request with [`BackendEvent::RequestFailed`]
    /// carrying `reason`, and one on a backend the resolution lacks fails as with
    /// [`AcpBackend::start`]. A `resolve` that panics resolves no backend.
    ///
    /// # Errors
    ///
    /// Returns [`AcpError::Runtime`] when the async runtime cannot be built and
    /// [`AcpError::Spawn`] when the resolver's or the runtime's thread cannot be spawned.
    pub fn start_deferred(
        resolve: impl FnOnce() -> Resolution + Send + 'static,
        tool_url: impl Fn(&SessionId, ToolRole) -> String + Send + Sync + 'static,
    ) -> Result<(Self, event_channel::UnboundedReceiver<BackendEvent>), AcpError> {
        let (resolved, resolution) = oneshot::channel();
        std::thread::Builder::new()
            .name(RESOLVE_THREAD_NAME.to_owned())
            .spawn(move || {
                // The runtime may have stopped already; then nobody needs the backends.
                let _ = resolved.send(resolve());
            })
            .map_err(AcpError::Spawn)?;
        Self::launch(resolution, tool_url)
    }

    /// Starts the runtime thread, which learns its backends from `resolution`.
    fn launch(
        resolution: oneshot::Receiver<Resolution>,
        tool_url: impl Fn(&SessionId, ToolRole) -> String + Send + Sync + 'static,
    ) -> Result<(Self, event_channel::UnboundedReceiver<BackendEvent>), AcpError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKER_THREADS)
            .thread_name(THREAD_NAME)
            .enable_all()
            .build()
            .map_err(AcpError::Runtime)?;
        let (commands, command_receiver) = mpsc::unbounded_channel();
        let (event_sender, event_receiver) = event_channel::unbounded();
        let connections = Connections::default();
        let thread = std::thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn({
                let connections = connections.clone();
                move || {
                    runtime.block_on(run(command_receiver, resolution, event_sender, connections));
                }
            })
            .map_err(AcpError::Spawn)?;
        Ok((
            Self {
                commands,
                connections,
                tool_url: Box::new(tool_url),
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

    /// Queues opening `start` on `backend`, with the URL of its tools.
    fn open(&self, backend: &BackendId, start: SessionStart) -> Result<(), BackendError> {
        let tools_url = (self.tool_url)(&start.session, start.role);
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
        role: ToolRole,
    ) -> Result<(), BackendError> {
        self.open(
            backend,
            SessionStart {
                request,
                session,
                open: Open::New,
                cwd: cwd.to_path_buf(),
                role,
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
        role: ToolRole,
    ) -> Result<(), BackendError> {
        self.open(
            backend,
            SessionStart {
                request,
                session,
                open: Open::Load(backend_session.clone()),
                cwd: cwd.to_path_buf(),
                role,
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

/// The runtime's running sessions: their routes, their tasks and who owns each task.
#[derive(Default)]
struct Sessions {
    /// The way to each session's current connection.
    routes: HashMap<SessionId, Route>,
    /// One task per opened session.
    tasks: JoinSet<()>,
    /// What each running task answers and reports through.
    owners: HashMap<task::Id, Owner>,
    /// One counter for every connection, so a reconnected session never repeats an id.
    permission_ids: PermissionIds,
}

impl Sessions {
    /// Opens `start` on the adapter `config` describes: claims the session's connection,
    /// which silences an older one, routes the session's commands to a new task running
    /// the connection, and remembers the task's owner. Replacing the older connection's
    /// route drops its sender, which closes it. `uninstructed` says the backend's
    /// instructions folder could not be written, so the session tells the user.
    fn spawn_session(
        &mut self,
        connections: &Connections,
        events: &Events,
        config: &Arc<AcpConfig>,
        start: SessionStart,
        tools_url: String,
        uninstructed: bool,
    ) {
        let reporter = connections.claim(start.session.clone(), events.clone());
        let (sender, receiver) = mpsc::unbounded_channel();
        let route = Route {
            generation: reporter.generation,
            sender,
        };
        self.routes.insert(start.session.clone(), route);
        let request = start.request;
        let task = self.tasks.spawn(connection::run_session(
            Arc::clone(config),
            start,
            receiver,
            reporter.clone(),
            Arc::clone(&self.permission_ids),
            tools_url,
            uninstructed,
        ));
        self.owners.insert(task.id(), Owner { request, reporter });
    }
}

/// An open the runtime holds until its backends are resolved.
struct Held {
    /// The backend whose adapter to start.
    backend: BackendId,
    /// The session to open.
    start: SessionStart,
    /// The URL of the session's tools.
    tools_url: String,
}

/// The runtime's main loop: spawns a task per opened session, routes commands to it and
/// settles tasks as they end. Opens are held until `resolution` delivers the backends and
/// then run in the order they arrived; a sender dropped without a resolution resolves no
/// backend. The backends' instructions folders are written as soon as they are known, on a
/// blocking thread so a slow data folder holds up none of the runtime's other tasks, and a
/// session on a backend whose folder could not be written is opened to tell the user so.
/// After [`Command::Shutdown`] (or once the backend is gone) it closes every session and
/// waits up to [`SHUTDOWN_GRACE`] for their tasks; no task runs before the backends are
/// resolved, so a shutdown before then ends the loop at once and drops the held opens.
async fn run(
    mut commands: mpsc::UnboundedReceiver<Command>,
    mut resolution: oneshot::Receiver<Resolution>,
    events: Events,
    connections: Connections,
) {
    let mut backends: Option<Backends> = None;
    // The instructions folders that could not be written, known with the backends.
    let mut unwritten = HashSet::new();
    let mut held = VecDeque::new();
    let mut sessions = Sessions::default();
    // Set once shutdown began: the moment the remaining sessions are abandoned.
    let mut deadline: Option<Instant> = None;
    loop {
        if let Some(backends) = &backends {
            while let Some(Held {
                backend,
                start,
                tools_url,
            }) = held.pop_front()
            {
                match backends.get(&backend) {
                    Some(Ok(config)) => {
                        let uninstructed = config
                            .instructions_dir
                            .as_ref()
                            .is_some_and(|dir| unwritten.contains(dir));
                        sessions.spawn_session(
                            &connections,
                            &events,
                            config,
                            start,
                            tools_url,
                            uninstructed,
                        );
                    }
                    Some(Err(reason)) => emit(&events, unresolved(reason, start)),
                    None => emit(&events, not_configured(&backend, start)),
                }
            }
        }
        if deadline.is_some() && sessions.tasks.is_empty() {
            break;
        }
        tokio::select! {
            // The resolution is taken before any command, so backends known at start hold
            // no open and a shutdown queued right behind an open does not drop it.
            biased;
            Some(ended) = sessions.tasks.join_next_with_id() => {
                settle(ended, &mut sessions.owners, &mut sessions.routes);
            }
            resolved = &mut resolution, if backends.is_none() => {
                let resolved = resolved.unwrap_or_else(|_| {
                    tracing::error!("resolving the ACP backends failed, none is configured");
                    Resolution::new()
                });
                let resolved: Backends = resolved
                    .into_iter()
                    .map(|(id, config)| (id, config.map(Arc::new)))
                    .collect();
                let written = resolved.clone();
                // The writes cannot panic, so the blocking task always returns its folders.
                unwritten = tokio::task::spawn_blocking(move || write_every_instructions(&written))
                    .await
                    .unwrap_or_default();
                backends = Some(resolved);
            }
            command = commands.recv(), if deadline.is_none() => match command {
                Some(Command::Open { backend, start, tools_url }) => {
                    held.push_back(Held { backend, start, tools_url });
                }
                Some(Command::Session { session, command }) => {
                    route(&mut sessions.routes, &session, command);
                }
                Some(Command::Close { session }) => {
                    held.retain(|open: &Held| open.start.session != session);
                    close(&mut sessions.routes, &connections, &session);
                }
                Some(Command::Shutdown) | None => {
                    close_all(&mut sessions.routes);
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

/// The event failing `start`, whose backend could not be resolved for `reason`.
fn unresolved(reason: &str, start: SessionStart) -> BackendEvent {
    BackendEvent::RequestFailed {
        request: start.request,
        session: start.session,
        error: reason.to_owned(),
    }
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
mod tests;
