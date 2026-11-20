//! The start of the widget engine, on a thread of its own.
//!
//! Starting CEF takes about a second on Linux, where its sandbox probe runs first, so the
//! binary starts it with [`Starting::spawn`] while the configuration is read and the backends
//! start, and [`Starting::join`]s it just before the window opens. A thread that could not be
//! spawned, or that panicked, counts as a failed start. When a [`Starting`] is dropped without
//! being joined, because the start failed before, it waits for the thread and drops what it
//! started, so the process never exits with CEF half started.

use std::io;
use std::thread::{self, JoinHandle};

use partitur_cef::CefError;

/// A start of the widget engine that may still run.
pub struct Starting<T>(Option<io::Result<JoinHandle<Result<T, CefError>>>>);

impl<T: Send + 'static> Starting<T> {
    /// Runs `start` on the thread `cef-start`.
    pub fn spawn(start: impl FnOnce() -> Result<T, CefError> + Send + 'static) -> Self {
        Self::new(thread::Builder::new().name("cef-start".into()).spawn(start))
    }
}

impl<T> Starting<T> {
    /// The start on `thread`, or the reason it could not be spawned.
    fn new(thread: io::Result<JoinHandle<Result<T, CefError>>>) -> Self {
        Self(Some(thread))
    }

    /// Waits for the start and returns what it ended with.
    ///
    /// # Errors
    ///
    /// Returns what the start failed with, and [`CefError::Thread`] when its thread could not
    /// be spawned or panicked.
    pub fn join(mut self) -> Result<T, CefError> {
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
fn wait<T>(thread: io::Result<JoinHandle<Result<T, CefError>>>) -> Result<T, CefError> {
    match thread {
        Ok(thread) => thread
            .join()
            .unwrap_or_else(|_| Err(CefError::Thread("it panicked".into()))),
        Err(error) => Err(CefError::Thread(error.to_string())),
    }
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
        let failed = CefError::Initialize("no CEF".into());
        let refused = failed.clone();
        assert_eq!(
            Starting::<()>::spawn(move || Err(refused)).join(),
            Err(failed)
        );
    }

    #[test]
    fn a_thread_that_could_not_be_spawned_fails_the_start() {
        let unspawned = Starting::<()>::new(Err(io::Error::other("no threads left")));
        assert_eq!(
            unspawned.join(),
            Err(CefError::Thread("no threads left".into()))
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
