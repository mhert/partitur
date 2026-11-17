//! Helpers shared by the binary's unit tests.
//!
//! [`processes`] serialises every test that writes an executable or starts a process. A process
//! forked while another test still has its freshly written script open for writing keeps that
//! file descriptor until it execs, and running the script then fails with "text file busy".
//! Tests run on parallel threads of one process, so the lock has to be one for the whole crate,
//! and every test here that writes or starts an executable must take it.

use std::sync::{Mutex, MutexGuard, PoisonError};

static PROCESSES: Mutex<()> = Mutex::new(());

/// Holds the crate-wide lock on writing and starting executables until the guard is dropped.
/// A test that panicked while holding it leaves it usable.
pub fn processes() -> MutexGuard<'static, ()> {
    PROCESSES.lock().unwrap_or_else(PoisonError::into_inner)
}
