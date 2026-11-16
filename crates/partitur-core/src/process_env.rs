//! The environment every child process gets, with no I/O of its own.
//!
//! A desktop app started from a launcher does not inherit the user's shell environment, and
//! asking the shell for it takes noticeable time. A `ProcessEnv` is a small handle that is
//! either known at once or filled in later by a background thread through its
//! `ProcessEnvSetter`. Threads that start child processes call `wait`, while the UI thread
//! calls `get`, which never blocks. The variables are added to the inherited environment, with
//! later entries winning; they never replace it.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

/// The variables added to a child process's inherited environment.
type Vars = Arc<[(String, String)]>;

type Shared = (Mutex<Option<Vars>>, Condvar);

fn lock(shared: &Shared) -> MutexGuard<'_, Option<Vars>> {
    shared.0.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A handle on the variables to add to every child process's environment.
#[derive(Clone, Debug)]
pub struct ProcessEnv(Arc<Shared>);

impl ProcessEnv {
    /// An environment that is known right now.
    #[must_use]
    pub fn ready(vars: Vec<(String, String)>) -> Self {
        Self(Arc::new((Mutex::new(Some(vars.into())), Condvar::new())))
    }

    /// An environment that is not known yet, and the setter that fills it in.
    ///
    /// Dropping the setter unset releases every waiter with no variables.
    #[must_use]
    pub fn pending() -> (Self, ProcessEnvSetter) {
        let env = Self(Arc::new((Mutex::new(None), Condvar::new())));
        let setter = ProcessEnvSetter(Some(env.clone()));
        (env, setter)
    }

    /// The variables if they are known, without blocking.
    #[must_use]
    pub fn get(&self) -> Option<Vars> {
        lock(&self.0).clone()
    }

    /// The variables, blocking the calling thread until they are set.
    #[must_use]
    pub fn wait(&self) -> Vars {
        let mut guard = lock(&self.0);
        loop {
            if let Some(vars) = guard.as_ref() {
                return Arc::clone(vars);
            }
            guard = self.0.1.wait(guard).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn fill(&self, vars: Vec<(String, String)>) {
        *lock(&self.0) = Some(vars.into());
        self.0.1.notify_all();
    }
}

/// Fills in a pending `ProcessEnv`. Dropped without `set`, it releases waiters with nothing.
#[derive(Debug)]
pub struct ProcessEnvSetter(Option<ProcessEnv>);

impl ProcessEnvSetter {
    /// Sets the variables and wakes every waiter.
    pub fn set(mut self, vars: Vec<(String, String)>) {
        if let Some(env) = self.0.take() {
            env.fill(vars);
        }
    }
}

impl Drop for ProcessEnvSetter {
    fn drop(&mut self) {
        if let Some(env) = self.0.take() {
            env.fill(Vec::new());
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;

    fn vars() -> Vec<(String, String)> {
        vec![("PATH".into(), "/opt/bin".into())]
    }

    #[test]
    fn a_ready_env_is_there_at_once() {
        let env = ProcessEnv::ready(vars());
        assert_eq!(env.get().unwrap().to_vec(), vars());
        assert_eq!(env.wait().to_vec(), vars());
    }

    #[test]
    fn a_pending_env_has_nothing_until_set() {
        let (env, setter) = ProcessEnv::pending();
        assert!(env.get().is_none());
        setter.set(vars());
        assert_eq!(env.get().unwrap().to_vec(), vars());
    }

    #[test]
    fn a_waiter_on_another_thread_gets_the_vars_once_set() {
        let (env, setter) = ProcessEnv::pending();
        let waiter = std::thread::spawn({
            let env = env.clone();
            move || env.wait()
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!waiter.is_finished());
        setter.set(vars());
        assert_eq!(waiter.join().unwrap().to_vec(), vars());
    }

    #[test]
    fn a_dropped_setter_releases_waiters_with_nothing() {
        let (env, setter) = ProcessEnv::pending();
        let waiter = std::thread::spawn({
            let env = env.clone();
            move || env.wait()
        });
        drop(setter);
        assert!(waiter.join().unwrap().is_empty());
        assert!(env.get().unwrap().is_empty());
    }

    #[test]
    fn a_set_setter_does_not_clear_the_vars_on_drop() {
        let (env, setter) = ProcessEnv::pending();
        setter.set(vars());
        assert_eq!(env.wait().to_vec(), vars());
    }

    #[test]
    fn a_poisoned_lock_is_recovered() {
        let env = ProcessEnv::ready(vars());
        let clone = env.clone();
        let _ = std::thread::spawn(move || {
            let _guard = clone.0.0.lock().unwrap();
            std::panic::resume_unwind(Box::new("poison"));
        })
        .join();
        assert_eq!(env.wait().to_vec(), vars());
    }
}
