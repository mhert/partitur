//! The start of the widget engine, on a thread of its own.
//!
//! Starting the widget engine can take a while, so the binary starts it with [`Starting::spawn`]
//! while the configuration is read and the backends start, and [`Starting::join`]s it just
//! before the window opens. A failed start is the reason, in words. A thread that could not be
//! spawned, or that panicked, counts as a failed start. When a [`Starting`] is dropped without
//! being joined, because the start failed before, it waits for the thread and drops what it
//! started, so the process never exits with the engine half started.

use std::io;
use std::thread::{self, JoinHandle};

/// A start of the widget engine that may still run.
pub struct Starting<T>(Option<io::Result<JoinHandle<Result<T, String>>>>);

impl<T: Send + 'static> Starting<T> {
    /// Runs `start` on the thread `cef-start`.
    pub fn spawn(start: impl FnOnce() -> Result<T, String> + Send + 'static) -> Self {
        Self::new(thread::Builder::new().name("cef-start".into()).spawn(start))
    }
}

impl<T> Starting<T> {
    /// The start on `thread`, or the reason it could not be spawned.
    fn new(thread: io::Result<JoinHandle<Result<T, String>>>) -> Self {
        Self(Some(thread))
    }

    /// Waits for the start and returns what it ended with.
    ///
    /// # Errors
    ///
    /// Returns what the start failed with, and why its thread failed when it could not be
    /// spawned or panicked.
    pub fn join(mut self) -> Result<T, String> {
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
fn wait<T>(thread: io::Result<JoinHandle<Result<T, String>>>) -> Result<T, String> {
    match thread {
        Ok(thread) => thread
            .join()
            .unwrap_or_else(|_| Err(thread_failed("it panicked"))),
        Err(error) => Err(thread_failed(&error.to_string())),
    }
}

/// Why the start failed when its thread failed for `reason`.
fn thread_failed(reason: &str) -> String {
    format!("the widget engine's thread failed: {reason}")
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
        let failed = "no CEF".to_owned();
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
            Err("the widget engine's thread failed: no threads left".into())
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
