//! The persistence port: what the core needs saved, without knowing where it goes.
//!
//! [`Store`] is the synchronous side, which [`App::load`](crate::App::load) reads the saved
//! state through before the window opens. [`StoreQueue`] is the command port every later
//! write goes through, like [`Git`](crate::Git): [`send`](StoreQueue::send) queues a
//! [`StoreCommand`] and returns at once, and the commands are carried out one after another
//! in the order they were queued, so a [`Load`](StoreCommand::Load) sees every write queued
//! before it. A write that fails, and every load, is reported later as a [`StoreEvent`].

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

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

    /// Loads the sessions of `project`, oldest first by creation time, ties by id. A session
    /// stored without its [`history`](Session::history) marks gets them from its history;
    /// one whose history cannot be read is loaded without them.
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

/// Correlates a [`StoreCommand::Load`] with the [`StoreEvent::Loaded`] that answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LoadId(pub u64);

/// One write or load queued through a [`StoreQueue`].
#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "almost every command is an append, which a box would only make allocate again"
)]
pub enum StoreCommand {
    /// Appends `record` to the log of `session`, a session of `project`, as
    /// [`Store::append_record`] does.
    Append {
        /// The project the session belongs to.
        project: ProjectId,
        /// The session whose log grows.
        session: SessionId,
        /// What is appended.
        record: TranscriptRecord,
    },
    /// Replaces the stored project list, as [`Store::save_projects`] does.
    SaveProjects(Vec<Project>),
    /// Replaces the stored sessions of `project`, as [`Store::save_sessions`] does.
    SaveSessions {
        /// The project whose sessions these are.
        project: ProjectId,
        /// Every session of the project.
        sessions: Vec<Session>,
    },
    /// Removes the stored data of `session`, as [`Store::delete_records`] does. An
    /// [`Append`](Self::Append) for the session queued after it is dropped, so it cannot bring
    /// the session's data back.
    Delete {
        /// The project the session belongs to.
        project: ProjectId,
        /// The session whose data goes.
        session: SessionId,
    },
    /// Loads every record of `session`, after every write queued before it; answered with
    /// [`StoreEvent::Loaded`].
    Load {
        /// Names the answer.
        request: LoadId,
        /// The project the session belongs to.
        project: ProjectId,
        /// The session whose log is read.
        session: SessionId,
    },
}

impl StoreCommand {
    /// The command's name in a [`StoreEvent::Failed`]: `append`, `save projects`,
    /// `save sessions`, `delete` or `load`.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Append { .. } => "append",
            Self::SaveProjects(_) => "save projects",
            Self::SaveSessions { .. } => "save sessions",
            Self::Delete { .. } => "delete",
            Self::Load { .. } => "load",
        }
    }

    /// The session the command is about, `None` for a list of projects or sessions.
    #[must_use]
    pub fn session(&self) -> Option<&SessionId> {
        match self {
            Self::Append { session, .. }
            | Self::Delete { session, .. }
            | Self::Load { session, .. } => Some(session),
            Self::SaveProjects(_) | Self::SaveSessions { .. } => None,
        }
    }
}

/// What a [`StoreQueue`] reports about the commands it carried out.
///
/// The errors are shared, so an event can be cloned; two events are equal when they say the
/// same, errors compared by their text.
#[derive(Debug, Clone)]
pub enum StoreEvent {
    /// A write failed. What it would have saved stays as it is in memory.
    Failed {
        /// The [`name`](StoreCommand::name) of the command that failed.
        command: &'static str,
        /// The session it was about, if any.
        session: Option<SessionId>,
        /// Why it failed.
        error: Arc<StoreError>,
    },
    /// A [`StoreCommand::Load`] was carried out.
    Loaded {
        /// The load's id.
        request: LoadId,
        /// The session whose log was read.
        session: SessionId,
        /// The records in the order they were appended, or why they could not be read.
        records: Result<Vec<TranscriptRecord>, Arc<StoreError>>,
    },
}

impl StoreEvent {
    /// The event reporting that `command` failed with `error`: a [`Failed`](Self::Failed) one
    /// for a write, and for a [`Load`](StoreCommand::Load) the [`Loaded`](Self::Loaded) one
    /// that answers it with the error, so whoever waits for the load hears of it.
    #[must_use]
    pub fn failed(command: &StoreCommand, error: StoreError) -> Self {
        match command {
            StoreCommand::Load {
                request, session, ..
            } => Self::Loaded {
                request: *request,
                session: session.clone(),
                records: Err(Arc::new(error)),
            },
            _ => Self::Failed {
                command: command.name(),
                session: command.session().cloned(),
                error: Arc::new(error),
            },
        }
    }
}

impl PartialEq for StoreEvent {
    fn eq(&self, other: &Self) -> bool {
        let same = |a: &StoreError, b: &StoreError| a.to_string() == b.to_string();
        match (self, other) {
            (
                Self::Failed {
                    command,
                    session,
                    error,
                },
                Self::Failed {
                    command: other_command,
                    session: other_session,
                    error: other_error,
                },
            ) => command == other_command && session == other_session && same(error, other_error),
            (
                Self::Loaded {
                    request,
                    session,
                    records,
                },
                Self::Loaded {
                    request: other_request,
                    session: other_session,
                    records: other_records,
                },
            ) => {
                request == other_request
                    && session == other_session
                    && match (records, other_records) {
                        (Ok(records), Ok(other)) => records == other,
                        (Err(error), Err(other)) => same(error, other),
                        _ => false,
                    }
            }
            _ => false,
        }
    }
}

/// The command port every write after start-up goes through, and the loads that must see
/// those writes.
///
/// [`send`](Self::send) returns at once. The commands are carried out in the order they were
/// sent, one after another; a failed write and every load is reported as a [`StoreEvent`]
/// through a channel of the implementation's own. A queue that can no longer carry out
/// commands reports each one it is sent as failed.
pub trait StoreQueue: Send {
    /// Queues `command`.
    fn send(&self, command: StoreCommand);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn io(text: &str) -> StoreError {
        StoreError::Unavailable(text.into())
    }

    fn loaded(
        session: &SessionId,
        records: Result<Vec<TranscriptRecord>, StoreError>,
    ) -> StoreEvent {
        StoreEvent::Loaded {
            request: LoadId(1),
            session: session.clone(),
            records: records.map_err(Arc::new),
        }
    }

    #[test]
    fn commands_name_themselves_and_their_session() {
        let (project, session) = (ProjectId::new(), SessionId::new());
        let append = StoreCommand::Append {
            project: project.clone(),
            session: session.clone(),
            record: TranscriptRecord::Notice("n".into()),
        };
        let delete = StoreCommand::Delete {
            project: project.clone(),
            session: session.clone(),
        };
        let load = StoreCommand::Load {
            request: LoadId(7),
            project: project.clone(),
            session: session.clone(),
        };
        let projects = StoreCommand::SaveProjects(Vec::new());
        let sessions = StoreCommand::SaveSessions {
            project,
            sessions: Vec::new(),
        };
        let named: Vec<_> = [&append, &projects, &sessions, &delete, &load]
            .iter()
            .map(|command| (command.name(), command.session().cloned()))
            .collect();
        assert_eq!(
            named,
            [
                ("append", Some(session.clone())),
                ("save projects", None),
                ("save sessions", None),
                ("delete", Some(session.clone())),
                ("load", Some(session)),
            ]
        );
    }

    #[test]
    fn a_failure_names_its_command_and_session() {
        let session = SessionId::new();
        let command = StoreCommand::Delete {
            project: ProjectId::new(),
            session: session.clone(),
        };
        let failed = StoreEvent::failed(&command, io("gone"));
        assert!(
            matches!(
                &failed,
                StoreEvent::Failed { command: "delete", session: Some(named), error }
                    if *named == session && error.to_string() == "gone"
            ),
            "{failed:?}"
        );
    }

    #[test]
    fn a_failed_load_answers_the_load() {
        let session = SessionId::new();
        let command = StoreCommand::Load {
            request: LoadId(1),
            project: ProjectId::new(),
            session: session.clone(),
        };

        assert_eq!(
            StoreEvent::failed(&command, io("stopped")),
            loaded(&session, Err(io("stopped")))
        );
    }

    #[test]
    fn events_are_equal_when_they_say_the_same() {
        let command = StoreCommand::SaveProjects(Vec::new());
        let failed = StoreEvent::failed(&command, io("full"));
        assert_eq!(failed, failed.clone());
        assert_eq!(failed, StoreEvent::failed(&command, io("full")));
        assert_ne!(failed, StoreEvent::failed(&command, io("other")));
        let projects = StoreCommand::SaveSessions {
            project: ProjectId::new(),
            sessions: Vec::new(),
        };
        assert_ne!(failed, StoreEvent::failed(&projects, io("full")));
        let delete = |session: &SessionId| StoreCommand::Delete {
            project: ProjectId::new(),
            session: session.clone(),
        };
        let first = SessionId::new();
        assert_ne!(
            StoreEvent::failed(&delete(&first), io("full")),
            StoreEvent::failed(&delete(&SessionId::new()), io("full"))
        );

        let session = SessionId::new();
        let records = |read| loaded(&session, read);
        let notice = vec![TranscriptRecord::Notice("n".into())];
        let loaded = records(Ok(notice.clone()));
        assert_eq!(loaded, records(Ok(notice)));
        assert_ne!(loaded, records(Ok(Vec::new())));
        assert_ne!(loaded, records(Err(io("bad"))));
        assert_eq!(records(Err(io("bad"))), records(Err(io("bad"))));
        assert_ne!(records(Err(io("bad"))), records(Err(io("worse"))));
        assert_ne!(records(Err(io("bad"))), records(Ok(Vec::new())));
        let read: Result<_, Arc<StoreError>> = Ok(vec![TranscriptRecord::Notice("n".into())]);
        let other_request = StoreEvent::Loaded {
            request: LoadId(2),
            session: session.clone(),
            records: read.clone(),
        };
        let other_session = StoreEvent::Loaded {
            request: LoadId(1),
            session: SessionId::new(),
            records: read,
        };
        assert_ne!(loaded, other_request);
        assert_ne!(loaded, other_session);
        assert_ne!(loaded, failed);
        assert_ne!(failed, loaded);
    }
}
