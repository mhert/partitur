//! The port through which [`App`](crate::App) learns the time.
//!
//! [`Clock`] answers with the current time in whole seconds since the Unix epoch. `App` stamps
//! creation times, last activity, archive marks and workflow events with it, so a test can hold
//! the time still or move it, and the core never reads the system clock itself. The binary
//! passes [`SystemClock`](crate::process::SystemClock).

/// A source of the current time.
pub trait Clock: Send {
    /// The current time in seconds since the Unix epoch.
    fn now(&self) -> u64;
}
