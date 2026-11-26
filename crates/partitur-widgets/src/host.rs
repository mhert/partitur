//! The Partitur side of the widget engine: [`EngineHost`] implements the [`WidgetHost`] port by
//! running the helper process and speaking the [`protocol`](crate::protocol) with it.
//!
//! [`start`] spawns the helper in a process group of its own, without a console window on
//! Windows, with its standard streams piped and Partitur's environment. It blocks until the
//! helper's [`FromEngine::Hello`] arrives, so it belongs on a thread of its own. The first start
//! adds no switch and waits [`Patience::sandboxed_hello`], since the helper probes its sandbox
//! before its hello. When that helper ends before its hello (its exit code [`PROBE_FAILED`]
//! included), sends none in time or says something else first, the host ends it, logs the
//! warning `CEF runs without the process sandbox: <reason>` and starts it once more with
//! [`NO_SANDBOX`], waiting [`Patience::hello`]. Only that second start's failure fails the start.
//! A helper that cannot be spawned or speaks another [`VERSION`] fails it at once, as does a
//! thread of the host that cannot start. Whenever a start fails, the helper's whole process tree
//! is ended.
//!
//! Three threads carry each helper's traffic, so the UI never waits on the pipe.
//! `widget-engine-out` writes the commands the [`WidgetHost`] methods queue, one to one, in the
//! order they were queued. `widget-engine-in` reads the helper's events: it paints each frame
//! into the [`LatestFrame`] of its view, which [`WidgetHost::open`] makes and
//! [`WidgetHost::close`] forgets, and announces it as a [`WidgetHostEvent::Frame`] only when the
//! paint found the view's damage empty. So however fast the helper paints, at most one frame
//! event per view waits, and a frame for a view that is not open is dropped. Every other event is
//! handed on as it comes. `widget-engine-log` hands each line the helper writes to its standard
//! error on to Partitur's log, at the info level under the target `partitur_widgets::engine`.
//!
//! A fourth thread, `widget-engine-watch`, supervises the running helper. When its output ends
//! or cannot be read, or it exits unasked, that is a crash: the host reports
//! [`WidgetHostEvent::EngineLost`], forgets every view's slot, ends the crashed helper's whole
//! process tree (also when the helper itself has exited and been reaped: its children may run
//! on, and a process group's id is not given to another while any member lives) and starts a new one with [`NO_PROBE`] (and [`NO_SANDBOX`] when the crashed one ran without
//! the sandbox), waiting [`Patience::hello`]. Its hello is reported as
//! [`WidgetHostEvent::EngineBack`]. When that start fails, or a later helper crashes too, the
//! host reports [`WidgetHostEvent::EngineStopped`] and starts no more. All of that happens on the
//! supervisor, never on the thread of a [`WidgetHost`] method. While no helper runs, `open` fails
//! with [`WidgetHostError::Unavailable`] and every other command is dropped; so is a command
//! whose helper's input is already gone. Nothing a crashed helper still sends is handed on.
//!
//! Dropping the host sends [`ToEngine::Shutdown`] and gives the helper [`Patience::close`] to
//! exit; a restart under way is broken off. A helper still running then has its whole process
//! tree ended. A helper that exited in order after that shutdown is the one never ended again,
//! since its pid may already name another process; a failed start's helper is ended like a
//! crashed one. Every wait for a helper's threads is bounded, since a process the helper
//! left behind may hold its pipes open.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use partitur_core::process::{kill_tree, no_window, own_group};
use partitur_core::widget_bridge::WidgetTheme;
use partitur_core::widget_host::{
    LatestFrame, ViewId, WidgetHost, WidgetHostError, WidgetHostEvent, WidgetInput, WidgetPage,
};
use thiserror::Error;

use crate::protocol::{
    FrameRegions, FromEngine, NO_PROBE, NO_SANDBOX, PROBE_FAILED, ProtocolError, ToEngine, VERSION,
    read_from_engine, write_to_engine,
};

/// How often the supervisor looks whether the running helper exited.
const POLL: Duration = Duration::from_millis(50);

/// How long a helper whose start failed gets to exit on its own, so its exit code can be read.
const EXIT: Duration = Duration::from_millis(500);

/// How long the host waits for an ended helper's threads before it leaves them behind.
const LINGER: Duration = Duration::from_secs(1);

/// How long the host waits for the helper at each step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Patience {
    /// For the hello of a helper that probes its sandbox first.
    pub sandboxed_hello: Duration,
    /// For the hello of a helper that does not probe.
    pub hello: Duration,
    /// For the helper to exit after [`ToEngine::Shutdown`].
    pub close: Duration,
}

impl Default for Patience {
    fn default() -> Self {
        Self {
            sandboxed_hello: Duration::from_secs(40),
            hello: Duration::from_secs(10),
            close: Duration::from_secs(5),
        }
    }
}

/// How to start the helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The helper's executable.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<OsString>,
    /// How long to wait for it.
    pub patience: Patience,
}

/// Why the helper did not start.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum EngineError {
    /// The helper could not be spawned.
    #[error("the widget engine could not be started: {0}")]
    Spawn(String),
    /// The helper sent no hello.
    #[error("the widget engine did not say hello: {0}")]
    NoHello(String),
    /// The helper exited with [`PROBE_FAILED`]: its sandbox probe failed.
    #[error("the widget engine's sandbox probe failed")]
    ProbeFailed,
    /// The helper speaks another protocol version, the one named.
    #[error("the widget engine speaks protocol version {0}, not {VERSION}")]
    Version(u32),
    /// The helper's first message could not be read.
    #[error(transparent)]
    Protocol(ProtocolError),
}

/// Whether a failed start with the sandbox may be the sandbox's doing, so that a start without
/// it may work.
fn sandbox_may_be_to_blame(error: &EngineError) -> bool {
    !matches!(error, EngineError::Spawn(_) | EngineError::Version(_))
}

/// The switches of a restart after a crash: no probe, and no sandbox unless the crashed helper
/// ran in it.
fn restart_switches(sandboxed: bool) -> Vec<&'static str> {
    let mut switches = vec![NO_PROBE];
    if !sandboxed {
        switches.push(NO_SANDBOX);
    }
    switches
}

/// Where the engine stands.
#[derive(Debug)]
enum State {
    /// A helper runs; `commands` queues for its writer.
    Running {
        sandboxed: bool,
        commands: Sender<ToEngine>,
    },
    /// The helper crashed and the supervisor starts another.
    Restarting,
    /// No helper runs, and none will.
    Stopped,
}

/// What the host and the helper's threads share, behind the host's lock.
#[derive(Debug)]
struct Inner {
    state: State,
    /// The start the running helper came from; its threads are the only ones heard.
    generation: u64,
    /// The slots of the open views.
    views: HashMap<ViewId, LatestFrame>,
}

impl Inner {
    /// Whether the helper of `generation` is the one running.
    fn current(&self, generation: u64) -> bool {
        matches!(self.state, State::Running { .. }) && self.generation == generation
    }
}

/// The host's lock and the sender of its events.
#[derive(Debug)]
struct Shared {
    inner: Mutex<Inner>,
    events: UnboundedSender<WidgetHostEvent>,
}

impl Shared {
    fn new(events: UnboundedSender<WidgetHostEvent>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                state: State::Stopped,
                generation: 0,
                views: HashMap::new(),
            }),
            events,
        }
    }

    /// The shared state, also after a thread panicked while holding it.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Puts `state` in place, for the helper of `generation`, forgets every view's slot and sends
    /// `event`, all under the lock, so no event of an earlier helper follows it. Returns the state
    /// it replaced.
    fn switch(&self, state: State, generation: u64, event: Option<WidgetHostEvent>) -> State {
        let mut inner = self.lock();
        inner.views.clear();
        inner.generation = generation;
        if let Some(event) = event {
            let _ = self.events.unbounded_send(event);
        }
        std::mem::replace(&mut inner.state, state)
    }

    /// Hands on what one message from the helper of `generation` means for the UI, if anything,
    /// while that helper is the one running.
    fn deliver(&self, message: FromEngine, generation: u64) {
        let event = match message {
            FromEngine::Frame(frame) => self.painted(&frame, generation),
            FromEngine::Cursor { view, cursor } => Some(WidgetHostEvent::Cursor { view, cursor }),
            FromEngine::Message { view, json } => Some(WidgetHostEvent::Message { view, json }),
            FromEngine::OpenLink { view, url } => Some(WidgetHostEvent::OpenLink { view, url }),
            FromEngine::Failed { view, error } => Some(WidgetHostEvent::Failed { view, error }),
            FromEngine::Hello { .. } => None,
        };
        if let Some(event) = event {
            let inner = self.lock();
            if inner.current(generation) {
                let _ = self.events.unbounded_send(event);
            }
        }
    }

    /// Paints `frame` into its view's slot, and the event announcing it when the slot's damage
    /// was empty; nothing for a view that is not open or a helper that is not the one running.
    fn painted(&self, frame: &FrameRegions, generation: u64) -> Option<WidgetHostEvent> {
        let latest = {
            let inner = self.lock();
            inner
                .current(generation)
                .then(|| inner.views.get(&frame.view).cloned())
                .flatten()?
        };
        let regions = frame
            .regions
            .iter()
            .map(|(region, rows)| (*region, rows.as_slice()));
        latest
            .paint_regions(frame.width, frame.height, regions)
            .then_some(WidgetHostEvent::Frame {
                view: frame.view,
                latest,
            })
    }
}

/// What reaches the supervisor.
#[derive(Debug)]
enum Notice {
    /// What the first message of the helper of `generation` says about its start.
    Hello {
        generation: u64,
        greeting: Result<bool, EngineError>,
    },
    /// The output of the helper of `generation` ended or broke after its hello.
    Ended { generation: u64 },
    /// The host was dropped.
    Shutdown,
}

impl Notice {
    /// The start of the helper it is about, if it is about one.
    fn generation(&self) -> Option<u64> {
        match self {
            Self::Hello { generation, .. } | Self::Ended { generation } => Some(*generation),
            Self::Shutdown => None,
        }
    }
}

/// One helper process and the threads that serve it. Dropping it ends the helper's whole process
/// tree unless the helper exited in order after a shutdown, then waits up to [`LINGER`] for the
/// threads, which end with the helper's streams.
#[derive(Debug)]
struct Engine {
    child: Child,
    threads: Vec<JoinHandle<()>>,
    /// Whether the helper exited after the host's own shutdown.
    orderly: bool,
}

impl Drop for Engine {
    fn drop(&mut self) {
        // A helper that crashed or exited unasked may leave its children behind, and the group's
        // id cannot name another group while any of them lives. A helper that exited in order
        // after a shutdown leaves none, and its reaped pid may already name another process.
        if !self.orderly {
            kill_tree(&mut self.child);
        }
        wait_for_threads(&self.threads, LINGER);
    }
}

/// A helper that said hello, before it is put in place.
struct Started {
    engine: Engine,
    sandboxed: bool,
    commands: Sender<ToEngine>,
}

/// Starts helpers and watches the running one; see the module documentation.
struct Supervisor {
    program: PathBuf,
    args: Vec<OsString>,
    patience: Patience,
    shared: Arc<Shared>,
    notify: Sender<Notice>,
    notices: Receiver<Notice>,
    /// The start of the newest helper.
    generation: u64,
}

impl Supervisor {
    /// Starts a helper with `switches` and waits up to `patience` for its hello.
    fn launch(&mut self, switches: &[&str], patience: Duration) -> Result<Started, EngineError> {
        self.generation += 1;
        let generation = self.generation;
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .args(switches)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        no_window(&mut command);
        own_group(&mut command);
        let program = &self.program;
        let spawn_error =
            |error: std::io::Error| EngineError::Spawn(format!("{}: {error}", program.display()));
        let child = command.spawn().map_err(spawn_error)?;
        // From here on every failure drops the engine, which ends the helper.
        let mut engine = Engine {
            child,
            threads: Vec::new(),
            orderly: false,
        };
        // Each stream was asked to be piped, so each is there.
        let (stdin, stdout, stderr) =
            pipes(&mut engine.child).ok_or_else(|| EngineError::Spawn("no pipes".into()))?;
        engine
            .threads
            .push(spawn("widget-engine-log", move || forward_log(stderr))?);
        let shared = Arc::clone(&self.shared);
        let notify = self.notify.clone();
        engine.threads.push(spawn("widget-engine-in", move || {
            read_events(stdout, generation, &shared, &notify);
        })?);
        let sandboxed = self.greeted(patience).map_err(|error| {
            if probe_failed(&mut engine.child) {
                EngineError::ProbeFailed
            } else {
                error
            }
        })?;
        let (commands, queued) = mpsc::channel();
        engine.threads.push(spawn("widget-engine-out", move || {
            write_commands(stdin, &queued);
        })?);
        Ok(Started {
            engine,
            sandboxed,
            commands,
        })
    }

    /// The first start: with the sandbox, then once more without it when the sandbox may be to
    /// blame.
    fn first_start(&mut self) -> Result<Started, EngineError> {
        match self.launch(&[], self.patience.sandboxed_hello) {
            Err(error) if sandbox_may_be_to_blame(&error) => {
                tracing::warn!("CEF runs without the process sandbox: {error}");
                self.launch(&[NO_SANDBOX], self.patience.hello)
            }
            started => started,
        }
    }

    /// What the newest helper's hello says, waiting up to `patience` for it.
    fn greeted(&self, patience: Duration) -> Result<bool, EngineError> {
        match self.next(patience) {
            Some(Notice::Hello { greeting, .. }) => greeting,
            // A reader ends only after its hello, so this is the host's shutdown.
            Some(_) => Err(EngineError::NoHello("the host shut down".into())),
            None => Err(EngineError::NoHello(format!("none within {patience:?}"))),
        }
    }

    /// The next notice about the newest helper or the host, waiting up to `patience`. Notices
    /// about earlier helpers are dropped.
    fn next(&self, patience: Duration) -> Option<Notice> {
        let deadline = Instant::now() + patience;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let notice = self.notices.recv_timeout(left).ok()?;
            if notice
                .generation()
                .is_none_or(|generation| generation == self.generation)
            {
                return Some(notice);
            }
        }
    }

    /// Puts `started` in place as the running helper, sending `event` with it.
    fn install(&self, started: Started, event: Option<WidgetHostEvent>) -> (Engine, bool) {
        let Started {
            engine,
            sandboxed,
            commands,
        } = started;
        let running = State::Running {
            sandboxed,
            commands,
        };
        self.shared.switch(running, self.generation, event);
        (engine, sandboxed)
    }

    /// The supervisor thread: watches the running helper and restarts it once after a crash,
    /// until the host is dropped or the engine stops.
    fn watch(mut self, mut engine: Engine, mut sandboxed: bool) {
        let mut restarted = false;
        loop {
            let crashed = match self.next(POLL) {
                Some(Notice::Shutdown) => return self.shut_down(engine),
                // The running helper's reader ended: its output closed or broke.
                Some(_) => true,
                None => exited(&mut engine.child)
                    .inspect(|status| tracing::warn!("the widget engine exited: {status}"))
                    .is_some(),
            };
            if !crashed {
                continue;
            }
            if restarted {
                let error = "the widget engine crashed again after its restart".to_owned();
                tracing::warn!("{error}");
                self.shared.switch(
                    State::Stopped,
                    self.generation,
                    Some(WidgetHostEvent::EngineStopped { error }),
                );
                return;
            }
            tracing::warn!("the widget engine crashed; it restarts");
            // Replacing the running state closes the crashed helper's queue, which ends its
            // writer.
            self.shared.switch(
                State::Restarting,
                self.generation,
                Some(WidgetHostEvent::EngineLost),
            );
            drop(engine);
            match self.launch(&restart_switches(sandboxed), self.patience.hello) {
                Ok(started) => {
                    (engine, sandboxed) = self.install(started, Some(WidgetHostEvent::EngineBack));
                    restarted = true;
                }
                Err(error) => {
                    tracing::warn!("the widget engine did not restart: {error}");
                    let error = error.to_string();
                    self.shared.switch(
                        State::Stopped,
                        self.generation,
                        Some(WidgetHostEvent::EngineStopped { error }),
                    );
                    return;
                }
            }
        }
    }

    /// Asks the running helper to shut down and gives it [`Patience::close`] to exit; dropping
    /// `engine` then ends what is left of it.
    fn shut_down(&self, mut engine: Engine) {
        let running = self.shared.switch(State::Stopped, self.generation, None);
        if let State::Running { commands, .. } = running {
            // The writer ends once the queue closes, after the commands still in it.
            let _ = commands.send(ToEngine::Shutdown);
        }
        engine.orderly = wait_for_exit(&mut engine.child, self.patience.close).is_some();
    }
}

/// The [`WidgetHost`] whose engine is the helper process; see the module documentation.
#[derive(Debug)]
pub struct EngineHost {
    shared: Arc<Shared>,
    notify: Sender<Notice>,
    watch: Option<JoinHandle<()>>,
}

/// Starts the helper as `launch` says and waits for its hello; see the module documentation.
/// Returns the host and the receiver of its events.
///
/// # Errors
///
/// Fails when the helper cannot be spawned or speaks another protocol version, or when neither
/// the start with the sandbox nor the one without it brings a hello in time.
pub fn start(
    launch: Launch,
) -> Result<(EngineHost, UnboundedReceiver<WidgetHostEvent>), EngineError> {
    let Launch {
        program,
        args,
        patience,
    } = launch;
    let (events, receiver) = unbounded();
    let shared = Arc::new(Shared::new(events));
    let (notify, notices) = mpsc::channel();
    let mut supervisor = Supervisor {
        program,
        args,
        patience,
        shared: Arc::clone(&shared),
        notify: notify.clone(),
        notices,
        generation: 0,
    };
    let started = supervisor.first_start()?;
    let (engine, sandboxed) = supervisor.install(started, None);
    // A supervisor that cannot start is dropped with its engine, which ends the helper.
    let watch = spawn("widget-engine-watch", move || {
        supervisor.watch(engine, sandboxed);
    })?;
    let host = EngineHost {
        shared,
        notify,
        watch: Some(watch),
    };
    Ok((host, receiver))
}

/// The helper's three piped streams.
fn pipes(child: &mut Child) -> Option<(ChildStdin, ChildStdout, ChildStderr)> {
    Some((
        child.stdin.take()?,
        child.stdout.take()?,
        child.stderr.take()?,
    ))
}

/// Starts a thread named `name` running `run`.
fn spawn(name: &str, run: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, EngineError> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(run)
        .map_err(|error| EngineError::Spawn(format!("no thread {name}: {error}")))
}

/// What the helper's first message says about the start: whether it runs sandboxed, or why it
/// failed.
fn greeting(first: Result<Option<FromEngine>, ProtocolError>) -> Result<bool, EngineError> {
    match first {
        Ok(Some(FromEngine::Hello {
            version: VERSION,
            sandboxed,
        })) => Ok(sandboxed),
        Ok(Some(FromEngine::Hello { version, .. })) => Err(EngineError::Version(version)),
        Ok(Some(_)) => Err(EngineError::NoHello("it spoke before its hello".into())),
        Ok(None) => Err(EngineError::NoHello("it ended before its hello".into())),
        Err(error) => Err(EngineError::Protocol(error)),
    }
}

/// The reader thread: hands the supervisor the greeting of the helper of `generation`, then
/// delivers events until the helper's output ends or breaks, and says so.
fn read_events(stdout: impl Read, generation: u64, shared: &Shared, notify: &Sender<Notice>) {
    let mut stdout = BufReader::new(stdout);
    let greeting = greeting(read_from_engine(&mut stdout));
    let started = greeting.is_ok();
    let _ = notify.send(Notice::Hello {
        generation,
        greeting,
    });
    if !started {
        return;
    }
    loop {
        match read_from_engine(&mut stdout) {
            Ok(Some(message)) => shared.deliver(message, generation),
            Ok(None) => break,
            Err(error) => {
                tracing::warn!("the widget engine's output could not be read: {error}");
                break;
            }
        }
    }
    let _ = notify.send(Notice::Ended { generation });
}

/// The writer thread: writes each queued command until the queue closes or the helper's input
/// breaks.
fn write_commands(stdin: impl Write, queued: &Receiver<ToEngine>) {
    let mut stdin = stdin;
    let _ = queued
        .iter()
        .all(|command| write_to_engine(&mut stdin, &command).is_ok());
}

/// The log thread: hands each line of `stderr` on to Partitur's log.
fn forward_log(stderr: impl Read) {
    for line in BufReader::new(stderr).split(b'\n').map_while(Result::ok) {
        let line = String::from_utf8_lossy(&line);
        tracing::info!(target: "partitur_widgets::engine", "{}", line.trim_end());
    }
}

/// How `child` exited, if it has.
fn exited(child: &mut Child) -> Option<ExitStatus> {
    child.try_wait().ok().flatten()
}

/// Waits up to `patience` for `child` to exit, and says how it did.
fn wait_for_exit(child: &mut Child, patience: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + patience;
    loop {
        let status = exited(child);
        if status.is_some() || deadline.saturating_duration_since(Instant::now()).is_zero() {
            return status;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Whether `child`, whose start failed, exits within [`EXIT`] with [`PROBE_FAILED`].
fn probe_failed(child: &mut Child) -> bool {
    wait_for_exit(child, EXIT).and_then(|status| status.code()) == Some(PROBE_FAILED)
}

/// Waits up to `patience` for every one of `threads` to finish. One still running then is left
/// behind.
fn wait_for_threads(threads: &[JoinHandle<()>], patience: Duration) {
    let deadline = Instant::now() + patience;
    while threads.iter().any(|thread| !thread.is_finished())
        && !deadline.saturating_duration_since(Instant::now()).is_zero()
    {
        std::thread::sleep(Duration::from_millis(5));
    }
}

impl EngineHost {
    /// Whether the running helper runs in CEF's sandbox; `false` while none runs.
    pub fn sandboxed(&self) -> bool {
        matches!(
            self.shared.lock().state,
            State::Running {
                sandboxed: true,
                ..
            }
        )
    }

    /// Queues `command` for the running helper's writer, or drops it while none runs.
    fn send(&self, command: ToEngine) {
        if let State::Running { commands, .. } = &self.shared.lock().state {
            let _ = commands.send(command);
        }
    }
}

impl WidgetHost for EngineHost {
    fn open(&self, view: ViewId, page: WidgetPage) -> Result<(), WidgetHostError> {
        let mut inner = self.shared.lock();
        let inner = &mut *inner;
        let State::Running { commands, .. } = &inner.state else {
            return Err(WidgetHostError::Unavailable(
                "the widget engine is not running".into(),
            ));
        };
        inner.views.insert(view, LatestFrame::default());
        let _ = commands.send(ToEngine::Open { view, page });
        Ok(())
    }

    fn post(&self, view: ViewId, json: String) -> Result<(), WidgetHostError> {
        self.send(ToEngine::Post { view, json });
        Ok(())
    }

    fn resize(
        &self,
        view: ViewId,
        width: u32,
        height: u32,
        scale: f32,
    ) -> Result<(), WidgetHostError> {
        self.send(ToEngine::Resize {
            view,
            width,
            height,
            scale,
        });
        Ok(())
    }

    fn set_visible(&self, view: ViewId, visible: bool) -> Result<(), WidgetHostError> {
        self.send(ToEngine::SetVisible { view, visible });
        Ok(())
    }

    fn input(&self, view: ViewId, input: WidgetInput) -> Result<(), WidgetHostError> {
        self.send(ToEngine::Input { view, input });
        Ok(())
    }

    fn set_theme(&self, view: ViewId, theme: WidgetTheme) -> Result<(), WidgetHostError> {
        self.send(ToEngine::SetTheme { view, theme });
        Ok(())
    }

    fn close(&self, view: ViewId) -> Result<(), WidgetHostError> {
        self.shared.lock().views.remove(&view);
        self.send(ToEngine::Close { view });
        Ok(())
    }
}

impl Drop for EngineHost {
    fn drop(&mut self) {
        let _ = self.notify.send(Notice::Shutdown);
        if let Some(watch) = self.watch.take() {
            let _ = watch.join();
        }
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

    use super::*;
    use partitur_core::widget_host::{Region, WidgetCursor};

    const VIEW: ViewId = ViewId(3);

    #[test]
    fn patience_defaults_to_forty_ten_and_five_seconds() {
        assert_eq!(
            Patience::default(),
            Patience {
                sandboxed_hello: Duration::from_secs(40),
                hello: Duration::from_secs(10),
                close: Duration::from_secs(5),
            }
        );
    }

    #[test]
    fn the_first_message_decides_the_start() {
        let hello = |version, sandboxed| Ok(Some(FromEngine::Hello { version, sandboxed }));
        assert_eq!(greeting(hello(VERSION, true)), Ok(true));
        assert_eq!(greeting(hello(VERSION, false)), Ok(false));
        assert_eq!(
            greeting(hello(VERSION + 1, true)),
            Err(EngineError::Version(VERSION + 1))
        );
        let early = FromEngine::Message {
            view: VIEW,
            json: "{}".into(),
        };
        assert!(matches!(
            greeting(Ok(Some(early))),
            Err(EngineError::NoHello(_))
        ));
        assert!(matches!(greeting(Ok(None)), Err(EngineError::NoHello(_))));
        assert_eq!(
            greeting(Err(ProtocolError::Truncated)),
            Err(EngineError::Protocol(ProtocolError::Truncated))
        );
    }

    #[test]
    fn only_a_missing_helper_or_another_version_is_no_sandboxes_doing() {
        assert!(!sandbox_may_be_to_blame(&EngineError::Spawn("gone".into())));
        assert!(!sandbox_may_be_to_blame(&EngineError::Version(VERSION + 1)));
        assert!(sandbox_may_be_to_blame(&EngineError::NoHello(
            "ended".into()
        )));
        assert!(sandbox_may_be_to_blame(&EngineError::ProbeFailed));
        assert!(sandbox_may_be_to_blame(&EngineError::Protocol(
            ProtocolError::Truncated
        )));
    }

    #[test]
    fn a_restart_skips_the_probe_and_keeps_an_unsandboxed_helper_unsandboxed() {
        assert_eq!(restart_switches(true), [NO_PROBE]);
        assert_eq!(restart_switches(false), [NO_PROBE, NO_SANDBOX]);
    }

    /// Shared state with the helper of `generation` running, and the receiver of its events.
    fn running(generation: u64) -> (Shared, UnboundedReceiver<WidgetHostEvent>) {
        let (events, receiver) = unbounded();
        let shared = Shared::new(events);
        let state = State::Running {
            sandboxed: true,
            commands: mpsc::channel().0,
        };
        shared.switch(state, generation, None);
        (shared, receiver)
    }

    fn received(receiver: &mut UnboundedReceiver<WidgetHostEvent>) -> Vec<WidgetHostEvent> {
        std::iter::from_fn(|| receiver.try_recv().ok()).collect()
    }

    #[test]
    fn every_event_but_a_frame_and_a_hello_is_handed_on_as_it_is() {
        let (shared, mut events) = running(1);
        for (message, event) in [
            (
                FromEngine::Cursor {
                    view: VIEW,
                    cursor: WidgetCursor::Text,
                },
                WidgetHostEvent::Cursor {
                    view: VIEW,
                    cursor: WidgetCursor::Text,
                },
            ),
            (
                FromEngine::Message {
                    view: VIEW,
                    json: "{}".into(),
                },
                WidgetHostEvent::Message {
                    view: VIEW,
                    json: "{}".into(),
                },
            ),
            (
                FromEngine::OpenLink {
                    view: VIEW,
                    url: "https://example.com".into(),
                },
                WidgetHostEvent::OpenLink {
                    view: VIEW,
                    url: "https://example.com".into(),
                },
            ),
            (
                FromEngine::Failed {
                    view: VIEW,
                    error: "boom".into(),
                },
                WidgetHostEvent::Failed {
                    view: VIEW,
                    error: "boom".into(),
                },
            ),
        ] {
            shared.deliver(message, 1);
            assert_eq!(received(&mut events), [event]);
        }
        let late_hello = FromEngine::Hello {
            version: VERSION,
            sandboxed: true,
        };
        shared.deliver(late_hello, 1);
        assert_eq!(received(&mut events), []);
    }

    fn frame(view: ViewId, byte: u8) -> FromEngine {
        FromEngine::Frame(FrameRegions {
            view,
            width: 1,
            height: 1,
            regions: vec![(Region::whole(1, 1), vec![byte; 4])],
        })
    }

    #[test]
    fn a_frame_is_painted_into_its_view_and_announced_once_until_taken() {
        let latest = LatestFrame::default();
        let (shared, mut events) = running(1);
        shared.lock().views.insert(VIEW, latest.clone());
        shared.deliver(frame(VIEW, 1), 1);
        assert_eq!(
            received(&mut events),
            [WidgetHostEvent::Frame {
                view: VIEW,
                latest: latest.clone()
            }]
        );
        shared.deliver(frame(VIEW, 2), 1);
        assert_eq!(received(&mut events), []);
        assert_eq!(latest.picture().bgra, vec![2; 4]);
        latest.take().unwrap();
        shared.deliver(frame(VIEW, 3), 1);
        assert_eq!(received(&mut events).len(), 1);
    }

    #[test]
    fn a_frame_for_a_view_that_is_not_open_is_dropped() {
        let (shared, mut events) = running(1);
        shared.deliver(frame(VIEW, 1), 1);
        assert_eq!(received(&mut events), []);
        assert!(shared.lock().views.is_empty());
    }

    #[test]
    fn nothing_of_a_helper_that_is_not_the_running_one_is_handed_on() {
        let latest = LatestFrame::default();
        let (shared, mut events) = running(2);
        shared.lock().views.insert(VIEW, latest.clone());
        let message = || FromEngine::Message {
            view: VIEW,
            json: "{}".into(),
        };
        shared.deliver(message(), 1);
        shared.deliver(frame(VIEW, 1), 1);
        assert_eq!(received(&mut events), []);
        assert_eq!(latest.size(), (0, 0));
        // Once the running helper is lost, not even its own messages count.
        shared.lock().state = State::Restarting;
        shared.deliver(message(), 2);
        shared.deliver(frame(VIEW, 1), 2);
        assert_eq!(received(&mut events), []);
        assert_eq!(latest.size(), (0, 0));
        shared.lock().state = State::Running {
            sandboxed: false,
            commands: mpsc::channel().0,
        };
        shared.deliver(message(), 2);
        assert_eq!(received(&mut events).len(), 1);
    }

    #[test]
    fn a_switch_forgets_every_view_and_sends_its_event() {
        let (shared, mut events) = running(1);
        shared.lock().views.insert(VIEW, LatestFrame::default());
        let replaced = shared.switch(State::Restarting, 1, Some(WidgetHostEvent::EngineLost));
        assert!(matches!(replaced, State::Running { .. }), "{replaced:?}");
        assert!(shared.lock().views.is_empty());
        assert_eq!(received(&mut events), [WidgetHostEvent::EngineLost]);
        assert!(matches!(shared.lock().state, State::Restarting));
    }

    #[test]
    fn a_shutdown_is_about_no_helper() {
        assert_eq!(Notice::Shutdown.generation(), None);
        assert_eq!(Notice::Ended { generation: 4 }.generation(), Some(4));
        let hello = Notice::Hello {
            generation: 5,
            greeting: Ok(true),
        };
        assert_eq!(hello.generation(), Some(5));
    }

    #[cfg(unix)]
    fn shell(script: &str) -> Child {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        own_group(&mut command).spawn().unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn a_probe_failed_only_when_the_helper_exits_with_its_code() {
        assert!(probe_failed(&mut shell(&format!("exit {PROBE_FAILED}"))));
        assert!(!probe_failed(&mut shell("exit 1")));
    }

    #[cfg(unix)]
    #[test]
    fn dropping_an_engine_ends_its_helper_and_waits_for_its_threads() {
        let joined = Arc::new(Mutex::new(false));
        let ran = Arc::clone(&joined);
        let thread = spawn("test-thread", move || {
            std::thread::sleep(Duration::from_millis(100));
            *ran.lock().unwrap() = true;
        })
        .unwrap();
        let mut child = shell("sleep 30");
        let pid = child.id().to_string();
        assert!(exited(&mut child).is_none());
        drop(Engine {
            child,
            threads: vec![thread],
            orderly: false,
        });
        assert!(*joined.lock().unwrap());
        assert!(!partitur_core::testing::alive(&pid));
    }

    #[test]
    fn the_wait_for_threads_ends_when_they_do_or_after_its_patience() {
        let thread = |patience| spawn("test-thread", move || std::thread::sleep(patience)).unwrap();
        let begun = Instant::now();
        let soon = thread(Duration::from_millis(100));
        wait_for_threads(&[soon], Duration::from_secs(5));
        let took = begun.elapsed();
        assert!(took >= Duration::from_millis(100), "{took:?}");
        assert!(took < Duration::from_secs(2), "{took:?}");
        let begun = Instant::now();
        let lingering = thread(Duration::from_secs(3));
        wait_for_threads(&[lingering], Duration::from_millis(100));
        let took = begun.elapsed();
        assert!(took >= Duration::from_millis(100), "{took:?}");
        assert!(took < Duration::from_secs(2), "{took:?}");
    }

    #[test]
    fn the_wait_for_an_exit_ends_when_the_helper_exits_or_after_its_patience() {
        let mut command = Command::new(std::env::current_exe().unwrap());
        // The test binary lists its tests and exits.
        command.arg("--list").stdout(Stdio::null());
        let mut quick = own_group(&mut command).spawn().unwrap();
        let begun = Instant::now();
        assert!(wait_for_exit(&mut quick, Duration::from_secs(10)).is_some());
        assert!(begun.elapsed() < Duration::from_secs(5));
        #[cfg(unix)]
        {
            let mut slow = shell("sleep 30");
            let begun = Instant::now();
            assert!(wait_for_exit(&mut slow, Duration::from_millis(100)).is_none());
            assert!(begun.elapsed() >= Duration::from_millis(100));
            kill_tree(&mut slow);
        }
    }

    /// A supervisor whose newest helper came from start `generation`, and its notifier.
    fn supervisor(generation: u64) -> (Supervisor, Sender<Notice>) {
        let (events, _) = unbounded();
        let (notify, notices) = mpsc::channel();
        let supervisor = Supervisor {
            program: PathBuf::from("nothing"),
            args: Vec::new(),
            patience: Patience::default(),
            shared: Arc::new(Shared::new(events)),
            notify: notify.clone(),
            notices,
            generation,
        };
        (supervisor, notify)
    }

    #[test]
    fn the_supervisor_hears_only_its_newest_helper_and_the_host() {
        let (supervisor, notify) = supervisor(2);
        notify.send(Notice::Ended { generation: 1 }).unwrap();
        notify.send(Notice::Ended { generation: 2 }).unwrap();
        notify.send(Notice::Shutdown).unwrap();
        let patience = Duration::from_secs(5);
        assert!(matches!(
            supervisor.next(patience),
            Some(Notice::Ended { generation: 2 })
        ));
        assert!(matches!(supervisor.next(patience), Some(Notice::Shutdown)));
        let begun = Instant::now();
        assert!(supervisor.next(Duration::from_millis(100)).is_none());
        assert!(begun.elapsed() >= Duration::from_millis(100));
    }

    #[test]
    fn a_hello_and_the_end_of_the_output_reach_the_supervisor() {
        use crate::protocol::write_from_engine;
        let mut output = Vec::new();
        let hello = FromEngine::Hello {
            version: VERSION,
            sandboxed: true,
        };
        write_from_engine(&mut output, &hello).unwrap();
        let message = FromEngine::Message {
            view: VIEW,
            json: "{}".into(),
        };
        write_from_engine(&mut output, &message).unwrap();
        let (shared, mut events) = running(7);
        let (notify, notices) = mpsc::channel();
        read_events(output.as_slice(), 7, &shared, &notify);
        let heard: Vec<Notice> = notices.try_iter().collect();
        assert!(
            matches!(
                heard.as_slice(),
                [
                    Notice::Hello {
                        generation: 7,
                        greeting: Ok(true)
                    },
                    Notice::Ended { generation: 7 }
                ]
            ),
            "{heard:?}"
        );
        assert_eq!(
            received(&mut events),
            [WidgetHostEvent::Message {
                view: VIEW,
                json: "{}".into()
            }]
        );
        // Output that breaks after the hello ends the reader too.
        let mut broken = Vec::new();
        write_from_engine(&mut broken, &hello).unwrap();
        broken.extend_from_slice(&[0, 0, 0, 9, 1]);
        read_events(broken.as_slice(), 7, &shared, &notify);
        assert_eq!(notices.try_iter().count(), 2);
        // A failed hello is all a reader says.
        read_events([].as_slice(), 7, &shared, &notify);
        let heard: Vec<Notice> = notices.try_iter().collect();
        assert!(
            matches!(
                heard.as_slice(),
                [Notice::Hello {
                    greeting: Err(_),
                    ..
                }]
            ),
            "{heard:?}"
        );
    }

    /// A group whose leader exits at once while a member it started sleeps on, and that
    /// member's pid.
    #[cfg(unix)]
    fn group_with_a_member_left() -> (Child, String) {
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 30 >/dev/null 2>&1 & echo $!"])
            .stdout(Stdio::piped());
        let mut leader = own_group(&mut command).spawn().unwrap();
        let mut member = String::new();
        leader
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut member)
            .unwrap_or_default();
        // The leader exits once it has named the member; reap it, as the exit poll does.
        assert!(wait_for_exit(&mut leader, Duration::from_secs(10)).is_some());
        (leader, member.trim().to_owned())
    }

    #[cfg(unix)]
    #[test]
    fn a_crashed_helper_that_was_reaped_still_has_its_group_ended() {
        let (child, member) = group_with_a_member_left();
        assert!(partitur_core::testing::alive(&member));
        drop(Engine {
            child,
            threads: Vec::new(),
            orderly: false,
        });
        // The member is no child of ours: it is gone once its new parent has reaped it.
        let reaped = (0..500).any(|_| {
            std::thread::sleep(Duration::from_millis(10));
            !partitur_core::testing::alive(&member)
        });
        assert!(reaped, "the group's member runs on");
    }

    #[cfg(unix)]
    #[test]
    fn a_helper_that_exited_in_order_is_never_signalled() {
        let (child, member) = group_with_a_member_left();
        drop(Engine {
            child,
            threads: Vec::new(),
            orderly: true,
        });
        let alive = partitur_core::testing::alive(&member);
        let _ = Command::new("kill").args(["-KILL", &member]).status();
        assert!(alive, "the group of an orderly helper was signalled");
    }
}
