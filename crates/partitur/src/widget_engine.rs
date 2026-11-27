//! The start of the widget engine, on a thread of its own.
//!
//! The widget engine is the helper process `partitur-widget-engine`, which [`locate`] looks for
//! beside the running executable and then in `../lib/partitur/`, where the packages install it.
//! [`start`] launches it through `partitur-widgets`. Off Linux it does not probe a sandbox, so
//! its first start gets the ordinary hello patience only ([`patience`]).
//!
//! Starting the widget engine can take a while, so the binary starts it with [`Starting::spawn`]
//! while the configuration is read and the backends start, and [`Starting::join`]s it just
//! before the window opens. A failed start is an [`EngineError`]. A thread that could not be
//! spawned, or that panicked, counts as a failed start. When a [`Starting`] is dropped without
//! being joined, because the start failed before, it waits for the thread and drops what it
//! started, so the process never exits with the engine half started.

use std::io;
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};

use futures::channel::mpsc::UnboundedReceiver;
use partitur_core::widget_host::WidgetHostEvent;
use partitur_widgets::host::{self, EngineError, EngineHost, Launch, Patience};

/// The file name of the helper on this platform.
const ENGINE: &str = if cfg!(windows) {
    "partitur-widget-engine.exe"
} else {
    "partitur-widget-engine"
};

/// The helper beside `exe`, else in `lib/partitur` next to the directory of `exe`, where
/// `exists` says it is.
pub fn locate(exe: &Path, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let beside = dir.join(ENGINE);
    if exists(&beside) {
        return Some(beside);
    }
    let packaged = dir.join("..").join("lib").join("partitur").join(ENGINE);
    exists(&packaged).then_some(packaged)
}

/// How long the host waits for the helper. On Linux the helper probes its sandbox first, so
/// the first start may take the default patience. Elsewhere there is no probe, and the first
/// start has the ten seconds of an ordinary hello.
pub fn patience(linux: bool) -> Patience {
    let default = Patience::default();
    if linux {
        default
    } else {
        Patience {
            sandboxed_hello: default.hello,
            ..default
        }
    }
}

/// How to start the helper found for `exe`.
fn launch(exe: &Path, exists: impl Fn(&Path) -> bool, linux: bool) -> Result<Launch, EngineError> {
    let program = locate(exe, exists).ok_or_else(|| {
        EngineError::Spawn(format!(
            "{ENGINE} is neither beside {} nor in ../lib/partitur",
            exe.display()
        ))
    })?;
    Ok(Launch {
        program,
        args: Vec::new(),
        patience: patience(linux),
    })
}

/// The receiver of the widget engine's events.
pub type Events = UnboundedReceiver<WidgetHostEvent>;

/// Starts the helper found for the running executable `exe`.
///
/// # Errors
///
/// Fails when no helper is found or it does not start; see [`host::start`].
pub fn start(exe: &Path) -> Result<(EngineHost, Events), EngineError> {
    host::start(launch(exe, Path::exists, cfg!(target_os = "linux"))?)
}

/// A start of the widget engine that may still run.
pub struct Starting<T>(Option<io::Result<JoinHandle<Result<T, EngineError>>>>);

impl<T: Send + 'static> Starting<T> {
    /// Runs `start` on the thread `widget-engine-start`.
    pub fn spawn(start: impl FnOnce() -> Result<T, EngineError> + Send + 'static) -> Self {
        Self::new(
            thread::Builder::new()
                .name("widget-engine-start".into())
                .spawn(start),
        )
    }
}

impl<T> Starting<T> {
    /// The start on `thread`, or the reason it could not be spawned.
    fn new(thread: io::Result<JoinHandle<Result<T, EngineError>>>) -> Self {
        Self(Some(thread))
    }

    /// Waits for the start and returns what it ended with.
    ///
    /// # Errors
    ///
    /// Returns what the start failed with, and why its thread failed when it could not be
    /// spawned or panicked.
    pub fn join(mut self) -> Result<T, EngineError> {
        // Only `join` and `drop` take the thread, and `join` consumes the start.
        let thread = self.0.take();
        wait(thread.unwrap_or_else(|| Err(io::Error::other("joined already"))))
    }
}

impl<T> Drop for Starting<T> {
    fn drop(&mut self) {
        if let Some(thread) = self.0.take() {
            drop(wait(thread));
        }
    }
}

/// Waits for `thread`, or returns why it could not be spawned.
fn wait<T>(thread: io::Result<JoinHandle<Result<T, EngineError>>>) -> Result<T, EngineError> {
    match thread {
        Ok(thread) => thread
            .join()
            .unwrap_or_else(|_| Err(thread_failed("it panicked"))),
        Err(error) => Err(thread_failed(&error.to_string())),
    }
}

/// The failed start when its thread failed for `reason`.
fn thread_failed(reason: &str) -> EngineError {
    EngineError::Spawn(format!("its thread failed: {reason}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_start_is_joined_with_its_outcome() {
        assert_eq!(Starting::spawn(|| Ok(7)).join(), Ok(7));
    }

    #[test]
    fn a_failed_engine_start_is_reported_as_such() {
        assert_eq!(
            Starting::<()>::spawn(|| Err(EngineError::Spawn("x".into()))).join(),
            Err(EngineError::Spawn("x".into()))
        );
    }

    #[test]
    fn a_panicking_start_fails() {
        let panicked = Starting::<()>::spawn(|| {
            std::panic::resume_unwind(Box::new("boom"));
        });
        assert_eq!(
            panicked.join(),
            Err(EngineError::Spawn("its thread failed: it panicked".into()))
        );
    }

    fn only(found: &'static str) -> impl Fn(&Path) -> bool {
        move |path| path == Path::new(found)
    }

    #[test]
    fn the_engine_beside_the_executable_is_found_first() {
        let beside = Path::new("/opt/p").join(ENGINE);
        assert_eq!(locate(Path::new("/opt/p/partitur"), |_| true), Some(beside));
    }

    #[test]
    fn the_engine_in_lib_partitur_is_found_next() {
        let packaged = Path::new("/usr/bin/../lib/partitur").join(ENGINE);
        let found = packaged.clone();
        assert_eq!(
            locate(Path::new("/usr/bin/partitur"), move |path| path == found),
            Some(packaged)
        );
    }

    #[test]
    fn no_engine_is_none() {
        assert_eq!(locate(Path::new("/usr/bin/partitur"), |_| false), None);
        assert_eq!(locate(Path::new("partitur"), only("/nowhere")), None);
        assert_eq!(locate(Path::new("/"), |_| true), None);
    }

    #[test]
    fn linux_probes_and_waits_longer_for_its_first_hello() {
        assert_eq!(patience(true), Patience::default());
    }

    #[test]
    fn off_linux_the_first_hello_has_the_ordinary_patience() {
        let patience = patience(false);
        assert_eq!(patience.sandboxed_hello, Patience::default().hello);
        assert_eq!(patience.hello, Patience::default().hello);
        assert_eq!(patience.close, Patience::default().close);
    }

    #[test]
    fn a_launch_names_the_found_engine_and_its_patience() {
        let found = launch(Path::new("/opt/p/partitur"), |_| true, false).unwrap();
        assert_eq!(found.program, Path::new("/opt/p").join(ENGINE));
        assert!(found.args.is_empty());
        assert_eq!(found.patience, patience(false));
        let linux = launch(Path::new("/opt/p/partitur"), |_| true, true).unwrap();
        assert_eq!(linux.patience, patience(true));
    }

    #[test]
    fn a_missing_engine_fails_the_launch_and_says_where_it_looked() {
        let reason = launch(Path::new("/opt/p/partitur"), |_| false, true)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(reason.starts_with("the widget engine could not be started"));
        assert!(reason.contains("/opt/p/partitur"));
        assert!(reason.contains("../lib/partitur"));
        assert!(start(Path::new("/nonexistent/partitur")).is_err());
    }

    #[test]
    fn a_thread_that_could_not_be_spawned_fails_the_start() {
        let unspawned = Starting::<()>::new(Err(io::Error::other("no threads left")));
        assert_eq!(
            unspawned.join(),
            Err(EngineError::Spawn(
                "its thread failed: no threads left".into()
            ))
        );
    }

    #[test]
    fn dropping_a_start_waits_for_it_and_drops_what_it_started() {
        struct Engine(Arc<AtomicBool>);
        impl Drop for Engine {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let engine = Engine(dropped.clone());
        let starting = Starting::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            Ok(engine)
        });
        drop(starting);
        assert!(dropped.load(Ordering::SeqCst));
    }
}
