//! Work the update loop hands to a thread of its own.
//!
//! [`off_loop`] runs a closure on a named thread and resolves, as a future a `Task` can
//! wait on, with what the closure returned. The update loop never waits for the work itself.
//! When the thread cannot start, or ends without a result because the closure panicked, the
//! future resolves with nothing, and each caller says what that means for it.

use std::thread;

use futures::channel::oneshot;

/// Runs `work` on a thread called `name` and resolves with its result, or with none when the
/// thread could not start or ended without one.
pub(crate) async fn off_loop<T: Send + 'static>(
    name: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (sender, receiver) = oneshot::channel();
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            // The receiver may be gone when the window closed meanwhile.
            let _ = sender.send(work());
        })
        .ok()?;
    receiver.await.ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic)]

    use futures::executor::block_on;

    use super::off_loop;

    #[test]
    fn the_work_runs_on_a_thread_of_the_given_name() {
        let name = block_on(off_loop("background-test", || {
            std::thread::current().name().map(str::to_owned)
        }));
        assert_eq!(name, Some(Some("background-test".to_owned())));
    }

    #[test]
    fn work_that_panics_resolves_with_nothing() {
        let done: Option<()> = block_on(off_loop("background-panic", || panic!("on purpose")));
        assert_eq!(done, None);
    }
}
