//! Reads a stored session's history without making the caller wait.
//!
//! [`FileReader`] is the [`TranscriptReader`] over the files [`FileStore`](crate::FileStore)
//! writes. Every read runs on a thread of its own, named `read-<n>`, so a long transcript holds
//! up nothing else. The thread parses the session's history file and hands the updates, or the
//! reason they could not be read, to the callback that came with the request. A session whose
//! history was never written reads as no updates, and a history whose first line is damaged
//! reads as corrupt. If no thread can be started, the callback gets an error at once, on the
//! caller's thread, so it runs exactly once either way.

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use partitur_core::{ProjectId, ReadDone, SessionId, StoreError, TranscriptReader};

use crate::file_store::read_history;
use crate::paths::StatePaths;

/// The [`TranscriptReader`] over the history files under a data folder.
#[derive(Debug)]
pub struct FileReader {
    paths: StatePaths,
    next: AtomicU64,
}

impl FileReader {
    /// A reader of the sessions stored under `paths`.
    #[must_use]
    pub fn new(paths: StatePaths) -> Self {
        Self {
            paths,
            next: AtomicU64::new(1),
        }
    }
}

impl TranscriptReader for FileReader {
    fn read(&self, project: ProjectId, session: SessionId, done: ReadDone) {
        let path = self.paths.history_file(&project, &session);
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        // The slot keeps `done` reachable if the thread cannot start: the closure is lost then.
        let slot = Arc::new(Mutex::new(Some(done)));
        let thread_slot = Arc::clone(&slot);
        let spawned = std::thread::Builder::new()
            .name(format!("read-{n}"))
            .spawn(move || {
                if let Some(done) = take(&thread_slot) {
                    done(read_history(&path));
                }
            })
            .map(drop);
        refuse_unstarted(spawned, &slot);
    }
}

/// Takes the callback out of its slot, once.
fn take(slot: &Mutex<Option<ReadDone>>) -> Option<ReadDone> {
    slot.lock().unwrap_or_else(PoisonError::into_inner).take()
}

/// Answers the callback in `slot` with an error when its thread did not start.
fn refuse_unstarted(spawned: io::Result<()>, slot: &Mutex<Option<ReadDone>>) {
    if let Err(error) = spawned
        && let Some(done) = take(slot)
    {
        done(Err(StoreError::Unavailable(format!(
            "could not start a thread to read the session: {error}"
        ))));
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
    use std::sync::mpsc;
    use std::time::Duration;

    use partitur_core::{
        BackendId, Fold, Scope, Session, Status, Store, StoreError, TitleSource, TranscriptRecord,
        Workspace,
    };

    use super::*;
    use crate::FileStore;

    fn read(
        reader: &FileReader,
        project: &ProjectId,
        session: &SessionId,
    ) -> (Result<Vec<TranscriptRecord>, StoreError>, String) {
        let (sender, receiver) = mpsc::channel();
        reader.read(
            project.clone(),
            session.clone(),
            Box::new(move |result| {
                let name = std::thread::current().name().unwrap_or_default().to_owned();
                sender.send((result, name)).unwrap();
            }),
        );
        receiver.recv_timeout(Duration::from_secs(5)).unwrap()
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
            parent: None,
            auto_allow: false,
            instructions_injected: false,
            created_at: 1,
            orchestrator: false,
            archived: None,
            workspace: Workspace::CheckedOut,
            child_defaults: None,
            first_prompt: String::new(),
            skill: None,
            title_source: TitleSource::Derived,
            last_activity: 0,
            children: Fold::Folded,
            workflow: None,
            question: None,
            wait_reported: None,
            seen_branch: None,
            resolved_cwd: None,
        }
    }

    #[test]
    fn a_reader_returns_what_the_store_appended() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StatePaths::under(dir.path());
        let store = FileStore::new(paths.clone());
        let project = ProjectId::new();
        let session = session(&project);
        store
            .save_sessions(project.clone(), std::slice::from_ref(&session))
            .unwrap();
        let first = TranscriptRecord::UserText("hello".into());
        let second = TranscriptRecord::AgentText("hi".into());
        store.append_record(session.id.clone(), &first).unwrap();
        store.append_record(session.id.clone(), &second).unwrap();

        let (result, thread) = read(&FileReader::new(paths), &project, &session.id);

        assert_eq!(result.unwrap(), vec![first, second]);
        assert!(thread.starts_with("read-"), "{thread}");
    }

    #[test]
    fn reads_run_on_numbered_threads() {
        let dir = tempfile::tempdir().unwrap();
        let reader = FileReader::new(StatePaths::under(dir.path()));
        let (project, session) = (ProjectId::new(), SessionId::new());

        let (_, one) = read(&reader, &project, &session);
        let (_, two) = read(&reader, &project, &session);

        assert_eq!((one.as_str(), two.as_str()), ("read-1", "read-2"));
    }

    #[test]
    fn a_missing_history_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let reader = FileReader::new(StatePaths::under(dir.path()));

        let (result, _) = read(&reader, &ProjectId::new(), &SessionId::new());

        assert_eq!(result.unwrap(), Vec::<TranscriptRecord>::new());
    }

    #[test]
    fn a_corrupt_history_reports_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StatePaths::under(dir.path());
        let (project, session) = (ProjectId::new(), SessionId::new());
        let file = paths.history_file(&project, &session);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "not json\n").unwrap();

        let (result, _) = read(&FileReader::new(paths), &project, &session);

        assert!(
            matches!(result, Err(StoreError::Corrupt { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn a_thread_that_did_not_start_answers_with_an_error() {
        let (sender, receiver) = mpsc::channel();
        let done: ReadDone = Box::new(move |result| sender.send(result).unwrap());
        let slot = Mutex::new(Some(done));

        refuse_unstarted(Err(io::Error::other("no threads")), &slot);

        let error = receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap_err();
        assert!(
            matches!(&error, StoreError::Unavailable(d) if d.contains("no threads")),
            "{error:?}"
        );
        assert!(take(&slot).is_none());
    }

    #[test]
    fn a_thread_that_started_leaves_the_callback_alone() {
        let done: ReadDone = Box::new(|_| unreachable!());
        let slot = Mutex::new(Some(done));

        refuse_unstarted(Ok(()), &slot);

        assert!(take(&slot).is_some());
    }
}
