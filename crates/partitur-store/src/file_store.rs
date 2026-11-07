//! The [`Store`] port on JSON and JSON-lines files.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use partitur_core::{Project, ProjectId, Session, SessionId, Store, StoreError, Update};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::paths::StatePaths;

/// A [`Store`] that keeps its data as files below one directory.
///
/// Projects and sessions are pretty-printed JSON files that are replaced atomically; a
/// session's updates are a JSON-lines file that only ever grows. See [`StatePaths`] for the
/// layout.
///
/// Because [`Store::append_update`] and [`Store::load_updates`] are given only a session id,
/// the store remembers which project each session belongs to. It learns that from
/// [`Store::load_sessions`] and [`Store::save_sessions`], so a session list has to be loaded
/// or saved before its updates are touched; any other session is reported as corrupt.
#[derive(Debug)]
pub struct FileStore {
    paths: StatePaths,
    sessions: Mutex<HashMap<SessionId, ProjectId>>,
}

impl FileStore {
    /// Creates a store on the layout of `paths`; nothing is touched until the first write.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::PathBuf;
    ///
    /// use partitur_core::{Project, ProjectId, Store};
    /// use partitur_store::{FileStore, StatePaths};
    ///
    /// let dir = tempfile::tempdir()?;
    /// let store = FileStore::new(StatePaths::under(dir.path()));
    /// let project = Project {
    ///     id: ProjectId::new(),
    ///     root: PathBuf::from("/work/shop"),
    ///     name: "shop".into(),
    ///     repositories: Vec::new(),
    ///     has_global_instructions: false,
    ///     has_global_claude_md: false,
    ///     closed: false,
    /// };
    ///
    /// store.save_projects(std::slice::from_ref(&project))?;
    ///
    /// assert_eq!(store.load_projects()?, vec![project]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new(paths: StatePaths) -> Self {
        Self {
            paths,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn known_sessions(&self) -> MutexGuard<'_, HashMap<SessionId, ProjectId>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn remember(&self, project: &ProjectId, sessions: &[Session]) {
        let mut known = self.known_sessions();
        for session in sessions {
            known.insert(session.id.clone(), project.clone());
        }
    }

    fn transcript_file(&self, session: &SessionId) -> Result<PathBuf, StoreError> {
        let project = self.known_sessions().get(session).cloned();
        match project {
            Some(project) => Ok(self.paths.transcript_file(&project, session)),
            None => Err(StoreError::Corrupt {
                path: self.paths.projects_dir(),
                detail: "unknown session".into(),
            }),
        }
    }
}

impl Store for FileStore {
    fn load_projects(&self) -> Result<Vec<Project>, StoreError> {
        read_list(&self.paths.projects_file())
    }

    fn save_projects(&self, projects: &[Project]) -> Result<(), StoreError> {
        write_list(&self.paths.projects_file(), projects)
    }

    fn load_sessions(&self, project: ProjectId) -> Result<Vec<Session>, StoreError> {
        let sessions = read_list(&self.paths.sessions_file(&project))?;
        self.remember(&project, &sessions);
        Ok(sessions)
    }

    fn save_sessions(&self, project: ProjectId, sessions: &[Session]) -> Result<(), StoreError> {
        // Remember first: the caller holds these sessions whether or not the write succeeds.
        self.remember(&project, sessions);
        write_list(&self.paths.sessions_file(&project), sessions)
    }

    fn append_update(&self, session: SessionId, update: &Update) -> Result<(), StoreError> {
        let path = self.transcript_file(&session)?;
        let mut line = serde_json::to_vec(update).map_err(corrupt(&path))?;
        line.push(b'\n');
        create_parent(&path)?;
        OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)
            .and_then(|mut file| {
                drop_partial_line(&mut file)?;
                file.write_all(&line)
            })
            .map_err(io_error(&path))
    }

    fn load_updates(&self, session: SessionId) -> Result<Vec<Update>, StoreError> {
        let path = self.transcript_file(&session)?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_error(&path)(error)),
        };
        let mut updates = Vec::new();
        for line in bytes.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            match serde_json::from_slice(line) {
                Ok(update) => updates.push(update),
                Err(error) if updates.is_empty() => return Err(corrupt(&path)(error)),
                // A line cut off by a crash ends the replay; what came before it is intact.
                Err(_) => break,
            }
        }
        Ok(updates)
    }
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> StoreError {
    move |source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn corrupt(path: &Path) -> impl FnOnce(serde_json::Error) -> StoreError {
    move |error| StoreError::Corrupt {
        path: path.to_path_buf(),
        detail: error.to_string(),
    }
}

/// Cuts a trailing line that lacks its newline off the file.
///
/// Every append writes one object and its newline in a single call, so a last line without a
/// newline is a write a crash or a full disk cut short. Appending after it would glue a valid
/// update onto the fragment and make the replay stop there for good.
fn drop_partial_line(file: &mut File) -> io::Result<()> {
    if file.metadata()?.len() == 0 {
        return Ok(());
    }
    let mut last = [0];
    file.seek(SeekFrom::End(-1))?;
    file.read_exact(&mut last)?;
    if last == [b'\n'] {
        return Ok(());
    }
    let mut bytes = Vec::new();
    file.rewind()?;
    file.read_to_end(&mut bytes)?;
    let complete = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |newline| newline + 1);
    file.set_len(complete as u64)
}

fn create_parent(path: &Path) -> Result<(), StoreError> {
    path.parent().map_or(Ok(()), |parent| {
        fs::create_dir_all(parent).map_err(io_error(parent))
    })
}

/// Reads a JSON list; a file that does not exist yet is an empty list.
fn read_list<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, StoreError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(corrupt(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(io_error(path)(error)),
    }
}

/// Replaces `path` with the JSON list, so that a crash leaves either the old file or the new
/// one: the data goes to a sibling `.tmp` file first and is renamed over the target.
fn write_list<T: Serialize>(path: &Path, items: &[T]) -> Result<(), StoreError> {
    let bytes = serde_json::to_vec_pretty(items).map_err(corrupt(path))?;
    let tmp = path.with_added_extension("tmp");
    create_parent(path)?;
    fs::write(&tmp, bytes).map_err(io_error(&tmp))?;
    fs::rename(&tmp, path).map_err(io_error(path))
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

    use partitur_core::{BackendId, Scope, Status};

    use super::*;

    fn store(dir: &Path) -> FileStore {
        FileStore::new(StatePaths::under(dir))
    }

    fn project(name: &str) -> Project {
        Project {
            id: ProjectId::new(),
            root: PathBuf::from("/work").join(name),
            name: name.into(),
            repositories: vec![partitur_core::Repository {
                id: partitur_core::RepositoryId::try_from("api").unwrap(),
                path: PathBuf::from("/work/api"),
                name: "api".into(),
                has_instructions: true,
                has_claude_md: true,
                is_root: false,
            }],
            has_global_instructions: true,
            has_global_claude_md: true,
            closed: false,
        }
    }

    fn session(project: &ProjectId, title: &str) -> Session {
        Session {
            id: SessionId::new(),
            project: project.clone(),
            scope: Scope::Project,
            cwd: PathBuf::from("/work"),
            title: title.into(),
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
        }
    }

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn projects_round_trip_through_json() {
        let dir = tempfile::tempdir().unwrap();
        let projects = vec![project("one"), project("two")];

        store(dir.path()).save_projects(&projects).unwrap();

        assert_eq!(store(dir.path()).load_projects().unwrap(), projects);
        let text = fs::read_to_string(dir.path().join("projects.json")).unwrap();
        assert!(text.contains("\n  "), "projects.json is pretty-printed");
    }

    #[test]
    fn sessions_round_trip_per_project() {
        let dir = tempfile::tempdir().unwrap();
        let first = ProjectId::new();
        let second = ProjectId::new();
        let mine = vec![session(&first, "a"), session(&first, "b")];
        let theirs = vec![session(&second, "c")];
        let writer = store(dir.path());

        writer.save_sessions(first.clone(), &mine).unwrap();
        writer.save_sessions(second.clone(), &theirs).unwrap();
        let reader = store(dir.path());
        assert_eq!(reader.load_sessions(first.clone()).unwrap(), mine);
        assert_eq!(reader.load_sessions(second.clone()).unwrap(), theirs);

        writer.save_sessions(first.clone(), &mine[..1]).unwrap();

        let reader = store(dir.path());
        assert_eq!(reader.load_sessions(first).unwrap(), &mine[..1]);
        assert_eq!(reader.load_sessions(second).unwrap(), theirs);
    }

    #[test]
    fn a_missing_file_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir.path().join("never-created"));
        let session = SessionId::new();

        assert!(store.load_projects().unwrap().is_empty());
        assert!(store.load_sessions(ProjectId::new()).unwrap().is_empty());
        assert!(
            store
                .save_sessions(ProjectId::new(), &[])
                .and_then(|()| store.load_updates(session))
                .is_err(),
            "an unsaved session is unknown, not empty"
        );
    }

    #[test]
    fn a_missing_transcript_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let project = ProjectId::new();
        let session = session(&project, "a");
        store
            .save_sessions(project, std::slice::from_ref(&session))
            .unwrap();

        assert!(store.load_updates(session.id).unwrap().is_empty());
    }

    #[test]
    fn an_empty_transcript_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let project = ProjectId::new();
        let session = session(&project, "a");
        store
            .save_sessions(project.clone(), std::slice::from_ref(&session))
            .unwrap();
        fs::write(
            StatePaths::under(dir.path()).transcript_file(&project, &session.id),
            "",
        )
        .unwrap();

        assert!(store.load_updates(session.id).unwrap().is_empty());
    }

    #[test]
    fn a_transcript_that_cannot_be_read_is_an_io_error_with_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let project = ProjectId::new();
        let session = session(&project, "a");
        store
            .save_sessions(project.clone(), std::slice::from_ref(&session))
            .unwrap();
        let file = StatePaths::under(dir.path()).transcript_file(&project, &session.id);
        fs::create_dir(&file).unwrap();

        let result = store.load_updates(session.id);

        assert!(matches!(
            result,
            Err(StoreError::Io { path, .. }) if path == file
        ));
    }

    #[test]
    fn malformed_json_is_reported_as_corrupt_with_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let project = ProjectId::new();
        let paths = StatePaths::under(dir.path());
        let sessions_file = paths.sessions_file(&project);
        fs::create_dir_all(sessions_file.parent().unwrap()).unwrap();
        fs::write(&sessions_file, "[{").unwrap();
        fs::write(paths.projects_file(), "not json").unwrap();
        let store = store(dir.path());

        assert!(matches!(
            store.load_projects(),
            Err(StoreError::Corrupt { path, detail })
                if path == paths.projects_file() && !detail.is_empty()
        ));
        assert!(matches!(
            store.load_sessions(project),
            Err(StoreError::Corrupt { path, .. }) if path == sessions_file
        ));
    }

    #[test]
    fn a_file_that_cannot_be_read_is_an_io_error_with_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StatePaths::under(dir.path());
        fs::create_dir(paths.projects_file()).unwrap();

        let result = store(dir.path()).load_projects();

        assert!(matches!(
            result,
            Err(StoreError::Io { path, .. }) if path == paths.projects_file()
        ));
    }

    #[test]
    fn a_data_directory_that_cannot_be_created_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "a file, not a folder").unwrap();
        let store = store(&blocker);
        let project = ProjectId::new();
        let session = session(&project, "a");

        let saved = store.save_projects(&[]);
        let sessions = store.save_sessions(project, std::slice::from_ref(&session));

        for result in [saved, sessions] {
            assert!(matches!(
                result,
                Err(StoreError::Io { path, .. }) if path.starts_with(&blocker)
            ));
        }
    }

    #[test]
    fn an_update_that_cannot_be_appended_is_an_io_error_with_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let project = ProjectId::new();
        let session = session(&project, "a");
        store
            .save_sessions(project.clone(), std::slice::from_ref(&session))
            .unwrap();
        let project_dir = StatePaths::under(dir.path())
            .sessions_file(&project)
            .parent()
            .unwrap()
            .to_path_buf();
        fs::remove_dir_all(&project_dir).unwrap();
        fs::write(&project_dir, "a file, not a folder").unwrap();

        let result = store.append_update(session.id, &Update::Notice("x".into()));

        assert!(matches!(
            result,
            Err(StoreError::Io { path, .. }) if path == project_dir
        ));
    }

    #[test]
    fn an_update_that_cannot_be_written_to_its_file_is_an_io_error_with_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let project = ProjectId::new();
        let session = session(&project, "a");
        store
            .save_sessions(project.clone(), std::slice::from_ref(&session))
            .unwrap();
        let file = StatePaths::under(dir.path()).transcript_file(&project, &session.id);
        fs::create_dir(&file).unwrap();

        let result = store.append_update(session.id, &Update::Notice("x".into()));

        assert!(matches!(
            result,
            Err(StoreError::Io { path, .. }) if path == file
        ));
    }

    #[test]
    fn updates_append_as_json_lines_and_replay_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let project = ProjectId::new();
        let session = session(&project, "a");
        let updates = vec![
            Update::UserText("hi\nthere".into()),
            Update::AgentText("hello".into()),
            Update::Usage { used: 1, size: 2 },
        ];
        let writer = store(dir.path());
        writer
            .save_sessions(project.clone(), std::slice::from_ref(&session))
            .unwrap();

        for update in &updates {
            writer.append_update(session.id.clone(), update).unwrap();
        }

        let file = StatePaths::under(dir.path()).transcript_file(&project, &session.id);
        let text = fs::read_to_string(file).unwrap();
        assert_eq!(text.lines().count(), 3);
        assert!(text.ends_with('\n'));
        let reader = store(dir.path());
        reader.load_sessions(project).unwrap();
        assert_eq!(reader.load_updates(session.id).unwrap(), updates);
    }

    /// A transcript with two updates, followed by `tail` as written by a crashed append.
    fn transcript_with_tail(dir: &Path, tail: &str) -> (FileStore, SessionId, PathBuf) {
        let project = ProjectId::new();
        let session = session(&project, "a");
        let store = store(dir);
        store
            .save_sessions(project.clone(), std::slice::from_ref(&session))
            .unwrap();
        store
            .append_update(session.id.clone(), &Update::UserText("a".into()))
            .unwrap();
        store
            .append_update(session.id.clone(), &Update::AgentText("b".into()))
            .unwrap();
        let file = StatePaths::under(dir).transcript_file(&project, &session.id);
        let mut text = fs::read_to_string(&file).unwrap();
        text.push_str(tail);
        fs::write(&file, text).unwrap();
        (store, session.id, file)
    }

    fn good_prefix() -> Vec<Update> {
        vec![Update::UserText("a".into()), Update::AgentText("b".into())]
    }

    #[test]
    fn a_malformed_tail_line_keeps_the_good_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let (store, session, _) =
            transcript_with_tail(dir.path(), "{\"AgentText\":\"cut off mid-wr");

        let updates = store.load_updates(session).unwrap();

        assert_eq!(updates, good_prefix());
    }

    #[test]
    fn a_malformed_line_in_the_middle_ends_the_replay() {
        let dir = tempfile::tempdir().unwrap();
        let (store, session, _) =
            transcript_with_tail(dir.path(), "garbage\n{\"UserText\":\"after the break\"}\n");

        let updates = store.load_updates(session).unwrap();

        assert_eq!(updates, good_prefix());
    }

    #[test]
    fn appending_after_a_truncated_tail_line_drops_the_fragment() {
        let dir = tempfile::tempdir().unwrap();
        let (store, session, file) =
            transcript_with_tail(dir.path(), "{\"AgentText\":\"cut off mid-wr");

        store
            .append_update(session.clone(), &Update::Notice("later".into()))
            .unwrap();

        let mut expected = good_prefix();
        expected.push(Update::Notice("later".into()));
        assert_eq!(store.load_updates(session).unwrap(), expected);
        let text = fs::read_to_string(file).unwrap();
        assert_eq!(text.lines().count(), 3);
        assert!(text.ends_with('\n'));
        assert!(!text.contains("cut off"));
    }

    #[test]
    fn appending_after_a_transcript_that_is_only_a_fragment_starts_it_over() {
        let dir = tempfile::tempdir().unwrap();
        let project = ProjectId::new();
        let session = session(&project, "a");
        let store = store(dir.path());
        store
            .save_sessions(project.clone(), std::slice::from_ref(&session))
            .unwrap();
        let file = StatePaths::under(dir.path()).transcript_file(&project, &session.id);
        fs::write(&file, "{\"UserText\":\"never fini").unwrap();

        store
            .append_update(session.id.clone(), &Update::Notice("x".into()))
            .unwrap();

        assert_eq!(
            store.load_updates(session.id).unwrap(),
            vec![Update::Notice("x".into())]
        );
        assert_eq!(fs::read_to_string(file).unwrap().lines().count(), 1);
    }

    #[test]
    fn a_failed_session_save_still_remembers_the_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "a file, not a folder").unwrap();
        let store = store(&blocker);
        let project = ProjectId::new();
        let session = session(&project, "a");

        let saved = store.save_sessions(project, std::slice::from_ref(&session));
        let appended = store.append_update(session.id, &Update::Notice("x".into()));

        assert!(matches!(saved, Err(StoreError::Io { .. })));
        assert!(matches!(appended, Err(StoreError::Io { .. })));
    }

    #[test]
    fn a_malformed_first_line_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let project = ProjectId::new();
        let session = session(&project, "a");
        let store = store(dir.path());
        store
            .save_sessions(project.clone(), std::slice::from_ref(&session))
            .unwrap();
        let file = StatePaths::under(dir.path()).transcript_file(&project, &session.id);
        fs::write(&file, "garbage\n{\"UserText\":\"a\"}\n").unwrap();

        let result = store.load_updates(session.id);

        assert!(matches!(
            result,
            Err(StoreError::Corrupt { path, .. }) if path == file
        ));
    }

    #[test]
    fn writes_are_atomic_no_tmp_file_remains() {
        let dir = tempfile::tempdir().unwrap();
        let project = project("p");
        let paths = StatePaths::under(dir.path());
        let store = store(dir.path());
        // A crash between writing the temporary file and renaming it leaves this behind.
        fs::write(dir.path().join("projects.json.tmp"), "{ half a wri").unwrap();
        store.save_projects(std::slice::from_ref(&project)).unwrap();
        store
            .save_sessions(project.id.clone(), &[session(&project.id, "a")])
            .unwrap();

        assert_eq!(
            file_names(dir.path()),
            vec!["projects".to_string(), "projects.json".to_string()]
        );
        assert_eq!(
            file_names(paths.sessions_file(&project.id).parent().unwrap()),
            vec!["sessions.json".to_string()]
        );
        assert_eq!(store.load_projects().unwrap(), vec![project]);
    }

    #[test]
    fn appending_for_an_unknown_session_is_corrupt_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let unknown = SessionId::new();

        let appended = store.append_update(unknown.clone(), &Update::Notice("x".into()));
        let loaded = store.load_updates(unknown);

        for result in [appended.map(|()| Vec::new()), loaded] {
            assert!(matches!(
                result,
                Err(StoreError::Corrupt { path, detail })
                    if path == dir.path().join("projects") && detail == "unknown session"
            ));
        }
        assert!(!dir.path().join("projects").exists());
    }

    #[test]
    fn a_fresh_store_learns_its_sessions_from_loading_them() {
        let dir = tempfile::tempdir().unwrap();
        let project = ProjectId::new();
        let session = session(&project, "a");
        store(dir.path())
            .save_sessions(project.clone(), std::slice::from_ref(&session))
            .unwrap();
        let reopened = store(dir.path());
        reopened.load_sessions(project).unwrap();

        reopened
            .append_update(session.id.clone(), &Update::Notice("x".into()))
            .unwrap();

        assert_eq!(
            reopened.load_updates(session.id).unwrap(),
            vec![Update::Notice("x".into())]
        );
    }

    #[test]
    fn a_poisoned_lock_does_not_stop_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let project = ProjectId::new();
        let session = session(&project, "a");
        std::thread::scope(|scope| {
            let _ = scope
                .spawn(|| {
                    let _held = store.sessions.lock().unwrap();
                    panic!("poison the lock");
                })
                .join();
        });
        assert!(store.sessions.is_poisoned());

        store
            .save_sessions(project, std::slice::from_ref(&session))
            .unwrap();

        store
            .append_update(session.id, &Update::Notice("x".into()))
            .unwrap();
    }
}
