//! Writes behind the caller: the [`StoreQueue`] over the files [`FileStore`] keeps.
//!
//! [`FileWriter`] runs one thread, `store-writer`, that carries out every queued
//! [`StoreCommand`] in the order it was sent, so a [`Load`](StoreCommand::Load) sees every
//! write queued before it. The thread keeps each session's history file open once it has
//! appended to it, up to [`OPEN_HISTORIES`] files, and closes a session's file before a
//! [`Delete`](StoreCommand::Delete) removes its folder. An append queued for a session after
//! its delete is dropped, so it cannot bring the session's folder back. A failed write, and
//! every load, is reported as a [`StoreEvent`] on the receiver [`FileWriter::start`] returns.
//!
//! [`FileWriter::store`] hands out a [`QueuedStore`], the synchronous [`Store`] over the same
//! files. Its reads and writes of projects, sessions and histories run on the writer's thread
//! too, in queue order, and the caller waits for them: a history it loads holds every record
//! queued before. Settings and attachment folders are not written by the queue, so they are
//! answered at once.
//!
//! Dropping the [`FileWriter`] carries out every command queued before and then joins the
//! thread. Should the thread panic, it reports one [`StoreEvent::Failed`] for the command
//! `writer`, since the commands still queued are lost, and stops. A command sent after that, through a [`QueuedStore`] still held, fails as
//! [`StoreError::Unavailable`].

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use partitur_core::{
    Project, ProjectId, ProjectSettings, Session, SessionId, Store, StoreCommand, StoreError,
    StoreEvent, StoreQueue, TranscriptRecord,
};

use crate::FileStore;
use crate::file_store::{history_line, io_error, open_history};

/// How many history files the writer keeps open at most. Opening one more closes them all
/// first; each is opened again on its next append.
pub const OPEN_HISTORIES: usize = 64;

/// What the writer's thread is asked to do.
#[expect(
    clippy::large_enum_variant,
    reason = "almost every job is an append, which a box would only make allocate again"
)]
enum Job {
    /// A queued command.
    Command(StoreCommand),
    /// A call of a [`QueuedStore`], whose caller waits for it.
    Call(Box<dyn FnOnce(&mut Writer) + Send>),
    /// The end: every job before it is done.
    Stop,
}

/// The [`StoreQueue`] that writes on a thread of its own; see the [module](self).
#[derive(Debug)]
pub struct FileWriter {
    jobs: Sender<Job>,
    events: UnboundedSender<StoreEvent>,
    store: Arc<FileStore>,
    thread: Option<JoinHandle<()>>,
}

impl FileWriter {
    /// Starts the writer's thread over `store`; every [`StoreEvent`] arrives on the returned
    /// receiver.
    ///
    /// # Errors
    ///
    /// Returns the [`io::Error`] of a thread that cannot be started.
    pub fn start(store: FileStore) -> io::Result<(Self, UnboundedReceiver<StoreEvent>)> {
        let (events, receiver) = unbounded();
        let (jobs, inbox) = mpsc::channel();
        let store = Arc::new(store);
        let mut writer = Writer {
            store: Arc::clone(&store),
            histories: HashMap::new(),
            deleted: HashSet::new(),
            events: events.clone(),
        };
        let thread = thread::Builder::new()
            .name("store-writer".into())
            .spawn(move || {
                let ran = panic::catch_unwind(AssertUnwindSafe(|| writer.run(&inbox)));
                if ran.is_err() {
                    let _ = writer.events.unbounded_send(panicked());
                }
            })?;
        let writer = Self {
            jobs,
            events,
            store,
            thread: Some(thread),
        };
        Ok((writer, receiver))
    }

    /// The synchronous [`Store`] over the same files, whose reads and writes wait their turn
    /// in this writer's queue.
    #[must_use]
    pub fn store(&self) -> QueuedStore {
        QueuedStore {
            jobs: self.jobs.clone(),
            store: Arc::clone(&self.store),
        }
    }
}

impl StoreQueue for FileWriter {
    fn send(&self, command: StoreCommand) {
        queue(&self.jobs, &self.events, command);
    }
}

impl Drop for FileWriter {
    /// Carries out every command queued so far, then joins the thread.
    fn drop(&mut self) {
        let _ = self.jobs.send(Job::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Queues `command` on `jobs`, or reports it as failed on `events` when the thread is gone.
fn queue(jobs: &Sender<Job>, events: &UnboundedSender<StoreEvent>, command: StoreCommand) {
    if let Err(mpsc::SendError(Job::Command(command))) = jobs.send(Job::Command(command)) {
        let _ = events.unbounded_send(StoreEvent::failed(&command, stopped()));
    }
}

/// The failure a writer that panicked reports, once: the commands still queued are lost.
fn panicked() -> StoreEvent {
    StoreEvent::Failed {
        command: "writer",
        session: None,
        error: Arc::new(StoreError::Unavailable(
            "the store's writer failed; writes still waiting were lost".into(),
        )),
    }
}

/// Why a command sent after the writer stopped fails.
fn stopped() -> StoreError {
    StoreError::Unavailable("the store's writer has stopped".into())
}

/// The synchronous [`Store`] of a [`FileWriter`]; see the [module](self).
#[derive(Debug, Clone)]
pub struct QueuedStore {
    jobs: Sender<Job>,
    store: Arc<FileStore>,
}

impl QueuedStore {
    /// Runs `work` on the writer's thread after every job queued before, and waits for it.
    fn call<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Writer) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        let (reply, answer) = mpsc::channel();
        let job = Job::Call(Box::new(move |writer| {
            let _ = reply.send(work(writer));
        }));
        let sent = self.jobs.send(job).ok();
        sent.and_then(|()| answer.recv().ok()).ok_or_else(stopped)?
    }
}

impl Store for QueuedStore {
    fn load_projects(&self) -> Result<Vec<Project>, StoreError> {
        self.call(|writer| writer.store.load_projects())
    }

    fn save_projects(&self, projects: &[Project]) -> Result<(), StoreError> {
        let projects = projects.to_vec();
        self.call(move |writer| writer.store.save_projects(&projects))
    }

    fn load_sessions(&self, project: ProjectId) -> Result<Vec<Session>, StoreError> {
        self.call(move |writer| writer.store.load_sessions(project))
    }

    fn save_sessions(&self, project: ProjectId, sessions: &[Session]) -> Result<(), StoreError> {
        let sessions = sessions.to_vec();
        self.call(move |writer| writer.store.save_sessions(project, &sessions))
    }

    fn append_record(
        &self,
        project: ProjectId,
        session: SessionId,
        record: &TranscriptRecord,
    ) -> Result<(), StoreError> {
        let record = record.clone();
        self.call(move |writer| writer.append(&project, &session, &record))
    }

    fn load_records(
        &self,
        project: ProjectId,
        session: SessionId,
    ) -> Result<Vec<TranscriptRecord>, StoreError> {
        self.call(move |writer| writer.store.load_records(project, session))
    }

    fn delete_records(&self, project: ProjectId, session: SessionId) -> Result<(), StoreError> {
        self.call(move |writer| writer.delete(project, session))
    }

    fn attachment_dir(
        &self,
        project: ProjectId,
        session: SessionId,
    ) -> Result<PathBuf, StoreError> {
        self.store.attachment_dir(project, session)
    }

    fn load_settings(&self, project: ProjectId) -> Result<ProjectSettings, StoreError> {
        self.store.load_settings(project)
    }
}

/// The state of the writer's thread.
struct Writer {
    store: Arc<FileStore>,
    /// The history files open for appending, by session.
    histories: HashMap<SessionId, File>,
    /// The sessions deleted, whose later appends are dropped.
    deleted: HashSet<SessionId>,
    events: UnboundedSender<StoreEvent>,
}

impl Writer {
    /// Carries out the jobs from `inbox` in order until [`Job::Stop`] or until every sender
    /// is gone.
    fn run(&mut self, inbox: &Receiver<Job>) {
        while let Ok(job) = inbox.recv() {
            match job {
                Job::Command(command) => self.carry_out(&command),
                Job::Call(work) => work(self),
                Job::Stop => break,
            }
        }
    }

    /// Carries out `command` and reports a failure or a load.
    fn carry_out(&mut self, command: &StoreCommand) {
        let done = match command {
            StoreCommand::Append {
                project,
                session,
                record,
            } => self.append(project, session, record),
            StoreCommand::SaveProjects(projects) => self.store.save_projects(projects),
            StoreCommand::SaveSessions { project, sessions } => {
                self.store.save_sessions(project.clone(), sessions)
            }
            StoreCommand::Delete { project, session } => {
                self.delete(project.clone(), session.clone())
            }
            StoreCommand::Load {
                request,
                project,
                session,
            } => {
                let records = self
                    .store
                    .load_records(project.clone(), session.clone())
                    .map_err(Arc::new);
                let _ = self.events.unbounded_send(StoreEvent::Loaded {
                    request: *request,
                    session: session.clone(),
                    records,
                });
                return;
            }
        };
        if let Err(error) = done {
            let _ = self
                .events
                .unbounded_send(StoreEvent::failed(command, error));
        }
    }

    /// Appends `record` to the history of `session` through its open file, opening it first
    /// when it is not open. A session deleted before is left alone. A file that fails a write
    /// is closed, so the next append opens it again and drops what the failure left.
    fn append(
        &mut self,
        project: &ProjectId,
        session: &SessionId,
        record: &TranscriptRecord,
    ) -> Result<(), StoreError> {
        if self.deleted.contains(session) {
            return Ok(());
        }
        self.store.raise_format()?;
        let path = self.store.paths().history_file(project, session);
        let line = history_line(&path, record)?;
        if self.histories.len() >= OPEN_HISTORIES && !self.histories.contains_key(session) {
            self.histories.clear();
        }
        let file = match self.histories.entry(session.clone()) {
            Entry::Occupied(open) => open.into_mut(),
            Entry::Vacant(closed) => closed.insert(open_history(&path)?),
        };
        let written = file.write_all(&line);
        if written.is_err() {
            self.histories.remove(session);
        }
        written.map_err(io_error(&path))
    }

    /// Closes the history of `session` and removes its stored data; later appends to it are
    /// dropped.
    fn delete(&mut self, project: ProjectId, session: SessionId) -> Result<(), StoreError> {
        self.histories.remove(&session);
        self.deleted.insert(session.clone());
        self.store.delete_records(project, session)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::fs;
    use std::path::Path;

    use partitur_core::{BackendId, LoadId, Scope, Status, Workspace};

    use super::*;
    use crate::StatePaths;

    fn project(root: &Path) -> Project {
        Project {
            id: ProjectId::new(),
            name: "p".into(),
            root: root.to_path_buf(),
            repositories: Vec::new(),
            instruction_files: Vec::new(),
            closed: false,
            fold: partitur_core::Fold::Folded,
        }
    }

    fn session(project: &ProjectId) -> Session {
        Session {
            id: SessionId::new(),
            project: project.clone(),
            scope: Scope::Project,
            cwd: "/work".into(),
            title: "t".into(),
            backend_session: None,
            backend: BackendId::try_from("claude").unwrap(),
            model: None,
            effort: None,
            status: Status::Idle,
            role: partitur_core::Role::Plain,
            auto_allow: false,
            instructions_injected: false,
            created_at: 1,
            archived: None,
            workspace: Workspace::CheckedOut,
            child_defaults: None,
            first_prompt: String::new(),
            skill: None,
            title_source: partitur_core::TitleSource::Derived,
            last_activity: 0,
            children: partitur_core::Fold::Folded,
            workflow: None,
            question: None,
            wait_reported: None,
            seen_branch: None,
            resolved_cwd: None,
            history: Some(partitur_core::HistoryMarks::default()),
        }
    }

    fn start(dir: &Path) -> (FileWriter, UnboundedReceiver<StoreEvent>) {
        FileWriter::start(FileStore::new(StatePaths::under(dir))).unwrap()
    }

    fn append(project: &ProjectId, session: &SessionId, text: &str) -> StoreCommand {
        StoreCommand::Append {
            project: project.clone(),
            session: session.clone(),
            record: TranscriptRecord::Notice(text.into()),
        }
    }

    /// Every event reported so far; the writer's queue is empty once a call returns.
    fn events(
        writer: &FileWriter,
        receiver: &mut UnboundedReceiver<StoreEvent>,
    ) -> Vec<StoreEvent> {
        let barrier = writer
            .store()
            .load_records(ProjectId::new(), SessionId::new());
        assert_eq!(barrier.unwrap(), []);
        std::iter::from_fn(|| receiver.try_recv().ok()).collect()
    }

    #[test]
    fn load_after_queued_append_sees_it() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, mut receiver) = start(dir.path());
        let (project, session) = (ProjectId::new(), SessionId::new());

        writer.send(append(&project, &session, "one"));
        writer.send(StoreCommand::Load {
            request: LoadId(3),
            project: project.clone(),
            session: session.clone(),
        });

        assert_eq!(
            events(&writer, &mut receiver),
            [StoreEvent::Loaded {
                request: LoadId(3),
                session,
                records: Ok(vec![TranscriptRecord::Notice("one".into())]),
            }]
        );
    }

    #[test]
    fn a_queued_append_raises_a_format_1_folder_to_format_2() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("format.json"), "{\"format\": 1}").unwrap();
        let (writer, mut receiver) = start(dir.path());

        writer.send(append(&ProjectId::new(), &SessionId::new(), "one"));

        assert_eq!(events(&writer, &mut receiver), []);
        assert_eq!(
            fs::read_to_string(dir.path().join("format.json")).unwrap(),
            "{\n  \"format\": 2\n}"
        );
    }

    #[test]
    fn a_queued_append_into_a_newer_format_fails() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("format.json"), "{\"format\": 3}").unwrap();
        let (writer, mut receiver) = start(dir.path());
        let (project, session) = (ProjectId::new(), SessionId::new());

        writer.send(append(&project, &session, "one"));

        assert_eq!(events(&writer, &mut receiver).len(), 1);
        assert!(
            !StatePaths::under(dir.path())
                .history_file(&project, &session)
                .exists()
        );
    }

    #[test]
    fn a_load_that_fails_reports_why() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, mut receiver) = start(dir.path());
        let (project, session) = (ProjectId::new(), SessionId::new());
        let history = StatePaths::under(dir.path()).history_file(&project, &session);
        fs::create_dir_all(history.parent().unwrap()).unwrap();
        fs::write(&history, "not json\n").unwrap();

        writer.send(StoreCommand::Load {
            request: LoadId(1),
            project,
            session,
        });

        let events = events(&writer, &mut receiver);
        assert!(
            matches!(
                events.as_slice(),
                [StoreEvent::Loaded { records: Err(error), .. }]
                    if matches!(**error, StoreError::Corrupt { .. })
            ),
            "{events:?}"
        );
    }

    #[test]
    fn dropping_the_store_flushes_queued_writes() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, _receiver) = start(dir.path());
        let (project, session) = (ProjectId::new(), SessionId::new());

        for n in 0..1000 {
            writer.send(append(&project, &session, &n.to_string()));
        }
        drop(writer);

        let store = FileStore::new(StatePaths::under(dir.path()));
        let records = store.load_records(project, session).unwrap();
        assert_eq!(records.len(), 1000);
        assert_eq!(records[999], TranscriptRecord::Notice("999".into()));
    }

    #[test]
    fn a_failed_append_reports_failed() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, mut receiver) = start(dir.path());
        let (project, session) = (ProjectId::new(), SessionId::new());
        // A file where the project's folder belongs, so the session's folder cannot be made.
        let folder = StatePaths::under(dir.path()).session_dir(&project, &session);
        fs::create_dir_all(folder.parent().unwrap().parent().unwrap()).unwrap();
        fs::write(folder.parent().unwrap(), "in the way").unwrap();

        writer.send(append(&project, &session, "lost"));

        let events = events(&writer, &mut receiver);
        assert!(
            matches!(
                events.as_slice(),
                [StoreEvent::Failed { command: "append", session: Some(failed), error }]
                    if *failed == session && matches!(**error, StoreError::Io { .. })
            ),
            "{events:?}"
        );
    }

    #[test]
    fn saves_and_deletes_are_carried_out_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, mut receiver) = start(dir.path());
        let project = project(dir.path());
        let kept = session(&project.id);
        let gone = session(&project.id);

        writer.send(StoreCommand::SaveProjects(vec![project.clone()]));
        writer.send(StoreCommand::SaveSessions {
            project: project.id.clone(),
            sessions: vec![kept.clone(), gone.clone()],
        });
        writer.send(append(&project.id, &gone.id, "before"));
        writer.send(StoreCommand::Delete {
            project: project.id.clone(),
            session: gone.id.clone(),
        });
        writer.send(StoreCommand::SaveSessions {
            project: project.id.clone(),
            sessions: vec![kept.clone()],
        });

        assert_eq!(events(&writer, &mut receiver), []);
        let store = writer.store();
        assert_eq!(
            store.load_projects().unwrap(),
            std::slice::from_ref(&project)
        );
        assert_eq!(store.load_sessions(project.id.clone()).unwrap(), [kept]);
        let folder = StatePaths::under(dir.path()).session_dir(&project.id, &gone.id);
        assert!(!folder.exists());
    }

    #[test]
    fn an_append_queued_after_a_delete_does_not_bring_the_session_back() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, mut receiver) = start(dir.path());
        let (project, session) = (ProjectId::new(), SessionId::new());
        let folder = StatePaths::under(dir.path()).session_dir(&project, &session);

        writer.send(append(&project, &session, "before"));
        writer.send(StoreCommand::Delete {
            project: project.clone(),
            session: session.clone(),
        });
        writer.send(append(&project, &session, "after"));

        assert_eq!(events(&writer, &mut receiver), []);
        assert!(!folder.exists());
    }

    #[test]
    fn failed_saves_and_deletes_report_their_command() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, mut receiver) = start(dir.path());
        let project = project(dir.path());
        let session = session(&project.id);
        let paths = StatePaths::under(dir.path());
        // A folder where the project list belongs, a file where the project's folder belongs,
        // and a folder where another session's record belongs, which no system removes as a
        // file. Windows finds no record below a file, so that cannot fail a delete there.
        fs::create_dir_all(paths.projects_file().join("x")).unwrap();
        fs::create_dir_all(paths.project_dir(&project.id).parent().unwrap()).unwrap();
        fs::write(paths.project_dir(&project.id), "in the way").unwrap();
        let (other, doomed) = (ProjectId::new(), SessionId::new());
        fs::create_dir_all(paths.meta_file(&other, &doomed).join("x")).unwrap();

        writer.send(StoreCommand::SaveProjects(vec![project.clone()]));
        writer.send(StoreCommand::SaveSessions {
            project: project.id.clone(),
            sessions: vec![session.clone()],
        });
        writer.send(StoreCommand::Delete {
            project: other,
            session: doomed.clone(),
        });

        let events = events(&writer, &mut receiver);
        assert!(
            matches!(
                events.as_slice(),
                [
                    StoreEvent::Failed { command: "save projects", session: None, .. },
                    StoreEvent::Failed { command: "save sessions", session: None, .. },
                    StoreEvent::Failed { command: "delete", session: Some(deleted), .. },
                ] if *deleted == doomed
            ),
            "{events:?}"
        );
    }

    #[test]
    fn the_queued_store_reads_and_writes_in_queue_order() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, mut receiver) = start(dir.path());
        let store = writer.store();
        let project = project(dir.path());
        let session = session(&project.id);

        store.save_projects(std::slice::from_ref(&project)).unwrap();
        store
            .save_sessions(project.id.clone(), std::slice::from_ref(&session))
            .unwrap();
        store
            .append_record(
                project.id.clone(),
                session.id.clone(),
                &TranscriptRecord::Notice("sync".into()),
            )
            .unwrap();
        writer.send(append(&project.id, &session.id, "queued"));

        assert_eq!(
            store
                .load_records(project.id.clone(), session.id.clone())
                .unwrap(),
            [
                TranscriptRecord::Notice("sync".into()),
                TranscriptRecord::Notice("queued".into()),
            ]
        );
        assert_eq!(
            store.load_projects().unwrap(),
            std::slice::from_ref(&project)
        );
        assert_eq!(
            store.load_sessions(project.id.clone()).unwrap(),
            std::slice::from_ref(&session)
        );
        assert_eq!(
            store
                .attachment_dir(project.id.clone(), session.id.clone())
                .unwrap(),
            StatePaths::under(dir.path())
                .session_dir(&project.id, &session.id)
                .join("attachments")
        );
        assert_eq!(
            store.load_settings(project.id.clone()).unwrap(),
            ProjectSettings::default()
        );
        let settings = StatePaths::under(dir.path()).settings_file(&project.id);
        fs::write(&settings, "[project]\nworktree_path = \"/wt/{branch}\"\n").unwrap();
        assert_ne!(
            store.load_settings(project.id.clone()).unwrap(),
            ProjectSettings::default()
        );
        store
            .delete_records(project.id.clone(), session.id.clone())
            .unwrap();
        assert!(
            store
                .load_records(project.id, session.id)
                .unwrap()
                .is_empty()
        );
        assert_eq!(events(&writer, &mut receiver), []);
    }

    #[test]
    fn after_the_writer_is_dropped_every_command_fails_as_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, _receiver) = start(dir.path());
        let store = writer.store();
        drop(writer);

        let error = store.load_projects().unwrap_err();
        assert!(matches!(error, StoreError::Unavailable(_)), "{error}");
        assert_eq!(error.to_string(), "the store's writer has stopped");

        let (jobs, inbox) = mpsc::channel();
        drop(inbox);
        let (events, mut receiver) = unbounded();
        let (project, session) = (ProjectId::new(), SessionId::new());
        queue(&jobs, &events, append(&project, &session, "late"));
        assert_eq!(
            receiver.try_recv().ok(),
            Some(StoreEvent::failed(
                &append(&project, &session, "late"),
                stopped()
            ))
        );
    }

    fn writer(dir: &Path) -> Writer {
        let (events, _receiver) = unbounded();
        Writer {
            store: Arc::new(FileStore::new(StatePaths::under(dir))),
            histories: HashMap::new(),
            deleted: HashSet::new(),
            events,
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_history_that_fails_a_write_is_closed() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = writer(dir.path());
        let (project, session) = (ProjectId::new(), SessionId::new());
        let history = StatePaths::under(dir.path()).history_file(&project, &session);
        fs::create_dir_all(history.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink("/dev/full", &history).unwrap();

        let error = writer
            .append(&project, &session, &TranscriptRecord::Notice("n".into()))
            .unwrap_err();

        assert!(
            matches!(error, StoreError::Io { ref path, .. } if *path == history),
            "{error}"
        );
        assert!(writer.histories.is_empty());
    }

    #[test]
    fn a_writer_that_panics_reports_it_and_fails_what_follows() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, mut receiver) = start(dir.path());
        let (project, session) = (ProjectId::new(), SessionId::new());

        writer
            .jobs
            .send(Job::Call(Box::new(|_| panic!("a bug in the writer"))))
            .unwrap();
        let store = writer.store();
        assert!(matches!(
            store.load_projects(),
            Err(StoreError::Unavailable(_))
        ));
        writer.send(append(&project, &session, "late"));
        writer.send(StoreCommand::Load {
            request: LoadId(9),
            project: project.clone(),
            session: session.clone(),
        });
        drop(writer);

        let events: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
        assert_eq!(
            events,
            [
                panicked(),
                StoreEvent::failed(&append(&project, &session, "late"), stopped()),
                // A load is answered all the same, so whoever waits for it hears.
                StoreEvent::Loaded {
                    request: LoadId(9),
                    session,
                    records: Err(Arc::new(stopped())),
                },
            ]
        );
    }

    #[test]
    fn the_writer_keeps_a_bounded_number_of_histories_open() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = writer(dir.path());
        let project = ProjectId::new();
        let record = TranscriptRecord::Notice("n".into());
        let first = SessionId::new();
        writer.append(&project, &first, &record).unwrap();
        for _ in 1..OPEN_HISTORIES {
            writer.append(&project, &SessionId::new(), &record).unwrap();
        }
        assert_eq!(writer.histories.len(), OPEN_HISTORIES);

        writer.append(&project, &first, &record).unwrap();
        assert_eq!(writer.histories.len(), OPEN_HISTORIES);
        writer.append(&project, &SessionId::new(), &record).unwrap();
        assert_eq!(writer.histories.len(), 1);

        writer.append(&project, &first, &record).unwrap();
        assert_eq!(
            writer.store.load_records(project, first).unwrap(),
            [record.clone(), record.clone(), record]
        );
    }
}
