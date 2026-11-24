//! The persistence port: what the core needs saved, without knowing where it goes.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

use crate::ids::{ProjectId, SessionId};
use crate::project::Project;
use crate::record::TranscriptRecord;
use crate::session::Session;
use crate::settings::ProjectSettings;

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
    /// The data was written by a newer Partitur than this one.
    #[error(
        "{} is in format {found}, but this version of Partitur only knows format {known}; update Partitur to open it",
        path.display()
    )]
    NewerFormat {
        /// The file that records the format.
        path: PathBuf,
        /// The format the data is in.
        found: u32,
        /// The newest format this version understands.
        known: u32,
    },
    /// The data cannot be read at all here, for the reason given.
    #[error("{0}")]
    Unavailable(String),
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

    /// Loads the sessions of `project`, oldest first by creation time, ties by id.
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

    /// Appends one record to the log of `session`, a session of `project`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the record cannot be written.
    fn append_record(
        &self,
        project: ProjectId,
        session: SessionId,
        record: &TranscriptRecord,
    ) -> Result<(), StoreError>;

    /// Loads every record of `session`, a session of `project`, in the order they were appended.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the log cannot be read or contains data that is not a
    /// record.
    fn load_records(
        &self,
        project: ProjectId,
        session: SessionId,
    ) -> Result<Vec<TranscriptRecord>, StoreError>;

    /// Removes the stored data of `session`, a session of `project`, with every update in it. Data that was never
    /// written is no error.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the log exists but cannot be removed.
    fn delete_records(&self, project: ProjectId, session: SessionId) -> Result<(), StoreError>;

    /// Names the folder where the attachments sent in `session`, a session of `project`, are kept. It creates
    /// nothing: the folder exists once something is written into it, and it goes away with
    /// the session's other data in [`delete_records`](Self::delete_records).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the folder cannot be named.
    fn attachment_dir(&self, project: ProjectId, session: SessionId)
    -> Result<PathBuf, StoreError>;

    /// Loads the settings of `project`; a project that has none yields the defaults.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the project's settings cannot be read or are invalid.
    fn load_settings(&self, project: ProjectId) -> Result<ProjectSettings, StoreError>;
}
