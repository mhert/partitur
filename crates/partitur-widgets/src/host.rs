//! The Partitur side of the widget engine: [`EngineHost`] implements the [`WidgetHost`] port by
//! running the helper process and speaking the [`protocol`](crate::protocol) with it.
//!
//! [`start`] spawns the helper in a process group of its own, without a console window on
//! Windows, with its standard streams piped and Partitur's environment. It blocks until the
//! helper's [`FromEngine::Hello`] arrives, so it belongs on a thread of its own. A helper that
//! cannot be spawned, says something else first, ends before its hello, sends none within
//! [`Patience::hello`] or speaks another [`VERSION`] fails the start, and so does a thread of
//! the host that cannot start. Either way the helper's whole process tree is ended.
//!
//! Three threads carry the traffic, so the UI never waits on the pipe. `widget-engine-out`
//! writes the commands the [`WidgetHost`] methods queue, one to one, in the order they were
//! queued. `widget-engine-in` reads the helper's events: it paints each frame into the
//! [`LatestFrame`] of its view, which [`WidgetHost::open`] makes and [`WidgetHost::close`]
//! forgets, and announces it as a [`WidgetHostEvent::Frame`] only when the paint found the
//! view's damage empty. So however fast the helper paints, at most one frame event per view
//! waits, and a frame for a view that is not open is dropped. Every other event is handed on
//! as it comes. `widget-engine-log` hands each line the helper writes to its standard error on
//! to Partitur's log, at the info level under the target `partitur_widgets::engine`.
//!
//! A command queued once the helper's input is gone fails with [`WidgetHostError::Send`].
//! Dropping the host sends [`ToEngine::Shutdown`] and gives the helper [`Patience::close`] to
//! exit. A helper still running then has its whole process tree ended. Then the threads are
//! joined.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
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
    FrameRegions, FromEngine, ProtocolError, ToEngine, VERSION, read_from_engine, write_to_engine,
};

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
    /// The helper speaks another protocol version, the one named.
    #[error("the widget engine speaks protocol version {0}, not {VERSION}")]
    Version(u32),
    /// The helper's first message could not be read.
    #[error(transparent)]
    Protocol(ProtocolError),
}

/// The slots of the open views, shared by the host and its reader.
type Views = Arc<Mutex<HashMap<ViewId, LatestFrame>>>;

/// The [`WidgetHost`] whose engine is the helper process; see the module documentation.
#[derive(Debug)]
pub struct EngineHost {
    commands: Sender<ToEngine>,
    views: Views,
    sandboxed: bool,
    child: Child,
    close: Duration,
    threads: Vec<JoinHandle<()>>,
}

/// Starts the helper as `launch` says and waits for its hello; see the module documentation.
/// Returns the host and the receiver of its events.
///
/// # Errors
///
/// Fails when the helper cannot be spawned, sends no hello in time, ends or says something else
/// before it, or speaks another protocol version.
pub fn start(
    launch: Launch,
) -> Result<(EngineHost, UnboundedReceiver<WidgetHostEvent>), EngineError> {
    let Launch {
        program,
        args,
        patience,
    } = launch;
    let mut command = Command::new(&program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_window(&mut command);
    own_group(&mut command);
    let spawn_error =
        |error: std::io::Error| EngineError::Spawn(format!("{}: {error}", program.display()));
    let child = command.spawn().map_err(spawn_error)?;
    let mut starting = Starting {
        child,
        threads: Vec::new(),
    };
    // Each stream was asked to be piped, so each is there.
    let pipes = pipes(&mut starting.child).ok_or_else(|| EngineError::Spawn("no pipes".into()));
    let (stdin, stdout, stderr) = starting.checked(pipes)?;
    let views = Views::default();
    let (events, receiver) = unbounded();
    let (hello, greeted) = mpsc::sync_channel(1);
    starting.thread("widget-engine-log", move || forward_log(stderr))?;
    let shared = Arc::clone(&views);
    starting.thread("widget-engine-in", move || {
        read_events(stdout, &hello, &shared, &events);
    })?;
    let greeting = greeted.recv_timeout(patience.hello).unwrap_or_else(|_| {
        Err(EngineError::NoHello(format!(
            "none within {:?}",
            patience.hello
        )))
    });
    let sandboxed = starting.checked(greeting)?;
    let (commands, queued) = mpsc::channel();
    starting.thread("widget-engine-out", move || write_commands(stdin, &queued))?;
    let host = EngineHost {
        commands,
        views,
        sandboxed,
        child: starting.child,
        close: patience.close,
        threads: starting.threads,
    };
    Ok((host, receiver))
}

/// A helper being started and the threads started for it so far.
struct Starting {
    child: Child,
    threads: Vec<JoinHandle<()>>,
}

impl Starting {
    /// Passes `result` on, after ending the helper's whole process tree and joining the threads
    /// when it is an error. Once the helper is gone its streams end, and with them the threads.
    fn checked<T>(&mut self, result: Result<T, EngineError>) -> Result<T, EngineError> {
        if result.is_err() {
            kill_tree(&mut self.child);
            for thread in self.threads.drain(..) {
                let _ = thread.join();
            }
        }
        result
    }

    /// Starts a thread named `name` running `run`, or ends the helper when it cannot start.
    fn thread(
        &mut self,
        name: &str,
        run: impl FnOnce() + Send + 'static,
    ) -> Result<(), EngineError> {
        let thread = self.checked(spawn(name, run))?;
        self.threads.push(thread);
        Ok(())
    }
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

/// The reader thread: hands the start its greeting, then delivers events until the helper's
/// output ends or breaks.
fn read_events(
    stdout: impl Read,
    hello: &SyncSender<Result<bool, EngineError>>,
    views: &Views,
    events: &UnboundedSender<WidgetHostEvent>,
) {
    let mut stdout = BufReader::new(stdout);
    let greeting = greeting(read_from_engine(&mut stdout));
    let started = greeting.is_ok();
    let _ = hello.send(greeting);
    if !started {
        return;
    }
    while let Ok(Some(message)) = read_from_engine(&mut stdout) {
        deliver(message, views, events);
    }
}

/// Hands on what one message from the helper means for the UI, if anything.
fn deliver(message: FromEngine, views: &Views, events: &UnboundedSender<WidgetHostEvent>) {
    let event = match message {
        FromEngine::Frame(frame) => painted(&frame, views),
        FromEngine::Cursor { view, cursor } => Some(WidgetHostEvent::Cursor { view, cursor }),
        FromEngine::Message { view, json } => Some(WidgetHostEvent::Message { view, json }),
        FromEngine::OpenLink { view, url } => Some(WidgetHostEvent::OpenLink { view, url }),
        FromEngine::Failed { view, error } => Some(WidgetHostEvent::Failed { view, error }),
        FromEngine::Hello { .. } => None,
    };
    if let Some(event) = event {
        let _ = events.unbounded_send(event);
    }
}

/// Paints `frame` into its view's slot, and the event announcing it when the slot's damage was
/// empty; nothing for a view that is not open.
fn painted(frame: &FrameRegions, views: &Views) -> Option<WidgetHostEvent> {
    let latest = lock(views).get(&frame.view).cloned()?;
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

/// The view slots, also after a thread panicked while holding them.
fn lock(views: &Views) -> MutexGuard<'_, HashMap<ViewId, LatestFrame>> {
    views.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Waits up to `patience` for `child` to exit, and says whether it did.
fn wait_for_exit(child: &mut Child, patience: Duration) -> bool {
    let deadline = Instant::now() + patience;
    while !deadline.saturating_duration_since(Instant::now()).is_zero() {
        if let Ok(Some(_)) = child.try_wait() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

impl EngineHost {
    /// Whether the helper runs in CEF's sandbox.
    pub fn sandboxed(&self) -> bool {
        self.sandboxed
    }

    /// Queues `command` for the writer.
    fn send(&self, command: ToEngine) -> Result<(), WidgetHostError> {
        self.commands
            .send(command)
            .map_err(|_| WidgetHostError::Send("the widget engine's input is gone".into()))
    }
}

impl WidgetHost for EngineHost {
    fn open(&self, view: ViewId, page: WidgetPage) -> Result<(), WidgetHostError> {
        lock(&self.views).insert(view, LatestFrame::default());
        self.send(ToEngine::Open { view, page })
    }

    fn post(&self, view: ViewId, json: String) -> Result<(), WidgetHostError> {
        self.send(ToEngine::Post { view, json })
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
        })
    }

    fn set_visible(&self, view: ViewId, visible: bool) -> Result<(), WidgetHostError> {
        self.send(ToEngine::SetVisible { view, visible })
    }

    fn input(&self, view: ViewId, input: WidgetInput) -> Result<(), WidgetHostError> {
        self.send(ToEngine::Input { view, input })
    }

    fn set_theme(&self, view: ViewId, theme: WidgetTheme) -> Result<(), WidgetHostError> {
        self.send(ToEngine::SetTheme { view, theme })
    }

    fn close(&self, view: ViewId) -> Result<(), WidgetHostError> {
        lock(&self.views).remove(&view);
        self.send(ToEngine::Close { view })
    }
}

impl Drop for EngineHost {
    fn drop(&mut self) {
        let _ = self.commands.send(ToEngine::Shutdown);
        // The writer ends once the queue closes, after the commands still in it.
        drop(std::mem::replace(&mut self.commands, mpsc::channel().0));
        // A helper that exited has been reaped, and its pid may already name another process.
        if !wait_for_exit(&mut self.child, self.close) {
            kill_tree(&mut self.child);
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
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

    fn delivered(message: FromEngine, views: &Views) -> Vec<WidgetHostEvent> {
        let (events, mut receiver) = unbounded();
        deliver(message, views, &events);
        drop(events);
        std::iter::from_fn(|| receiver.try_recv().ok()).collect()
    }

    #[test]
    fn every_event_but_a_frame_and_a_hello_is_handed_on_as_it_is() {
        let views = Views::default();
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
            assert_eq!(delivered(message, &views), [event]);
        }
        let late_hello = FromEngine::Hello {
            version: VERSION,
            sandboxed: true,
        };
        assert_eq!(delivered(late_hello, &views), []);
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
        let views = Views::default();
        lock(&views).insert(VIEW, latest.clone());
        assert_eq!(
            delivered(frame(VIEW, 1), &views),
            [WidgetHostEvent::Frame {
                view: VIEW,
                latest: latest.clone()
            }]
        );
        assert_eq!(delivered(frame(VIEW, 2), &views), []);
        assert_eq!(latest.picture().bgra, vec![2; 4]);
        latest.take().unwrap();
        assert_eq!(delivered(frame(VIEW, 3), &views).len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_failure_while_starting_ends_the_helper_and_joins_its_threads() {
        let mut command = Command::new("sleep");
        command.arg("30");
        let child = own_group(&mut command).spawn().unwrap();
        let joined = Arc::new(Mutex::new(false));
        let ran = Arc::clone(&joined);
        let mut starting = Starting {
            child,
            threads: Vec::new(),
        };
        starting
            .thread("test-thread", move || *ran.lock().unwrap() = true)
            .unwrap();
        let error = EngineError::Spawn("no thread".into());
        assert_eq!(starting.checked::<()>(Err(error.clone())), Err(error));
        assert!(starting.threads.is_empty());
        assert!(*joined.lock().unwrap());
        assert!(starting.child.try_wait().unwrap().is_some());
    }

    #[test]
    fn a_frame_for_a_view_that_is_not_open_is_dropped() {
        let views = Views::default();
        assert_eq!(delivered(frame(VIEW, 1), &views), []);
        assert!(lock(&views).is_empty());
    }
}
