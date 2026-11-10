//! The persistence port: what the core needs saved, without knowing where it goes.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

use crate::backend::Update;
use crate::ids::{ProjectId, SessionId};
use crate::project::Project;
use crate::session::Session;

/// Why the store could not read or write its data.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StoreError {
    /// Reading or writing the underlying storage failed.
    #[error("cannot access {}: {source}", path.display())]
    Io {
        /// The file or folder being accessed.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// Stored data exists but cannot be understood.
    #[error("{} is corrupt: {detail}", path.display())]
    Corrupt {
        /// The file holding the bad data.
        path: PathBuf,
        /// What is wrong with it.
        detail: String,
    },
}

/// Persists projects, their sessions and each session's update log.
///
/// The trait is object-safe and is used as `Box<dyn Store>`. Saving replaces what was stored
/// before; loading something never saved yields an empty list.
pub trait Store: Send {
    /// Loads the projects the user has open, in the order they were opened.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the stored list cannot be read.
    fn load_projects(&self) -> Result<Vec<Project>, StoreError>;

    /// Replaces the stored project list.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the list cannot be written.
    fn save_projects(&self, projects: &[Project]) -> Result<(), StoreError>;

    /// Loads the sessions of `project`, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the stored list cannot be read.
    fn load_sessions(&self, project: ProjectId) -> Result<Vec<Session>, StoreError>;

    /// Replaces the stored sessions of `project`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the list cannot be written.
    fn save_sessions(&self, project: ProjectId, sessions: &[Session]) -> Result<(), StoreError>;

    /// Appends one update to the log of `session`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the update cannot be written.
    fn append_update(&self, session: SessionId, update: &Update) -> Result<(), StoreError>;

    /// Loads every update of `session`, in the order they were appended.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the log cannot be read or contains data that is not an
    /// update.
    fn load_updates(&self, session: SessionId) -> Result<Vec<Update>, StoreError>;

    /// Removes the log of `session` with every update in it. A log that was never written
    /// is no error; loading it afterwards yields an empty list.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the log exists but cannot be removed.
    fn delete_updates(&self, session: SessionId) -> Result<(), StoreError>;
}
