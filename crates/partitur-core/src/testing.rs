//! In-memory stand-ins for the [`Backend`] and [`Store`] ports, for tests in this and other
//! crates. Available with the `testing` cargo feature.
//!
//! Both fakes are cheap handles to shared state: keep a clone, hand the other to
//! [`App`](crate::App), and inspect or steer the fake while the app owns it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::backend::{Backend, BackendError, PromptContent, Update};
use crate::ids::{
    BackendId, BackendInfo, BackendSessionId, ConfigId, ConfigValueId, PermissionId,
    PermissionOptionId, ProjectId, RequestId, SessionId,
};
use crate::project::Project;
use crate::session::Session;
use crate::store::{Store, StoreError};

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The backend list and default of a setup with one backend, `claude`, running
/// `claude-agent-acp`: what [`App::load`](crate::App::load) gets when nothing else is
/// configured.
///
/// # Examples
///
/// ```
/// use partitur_core::testing::single_claude;
///
/// let (backends, default) = single_claude();
/// assert_eq!(backends.len(), 1);
/// assert_eq!(backends[0].id, default);
/// assert_eq!(default.as_str(), "claude");
/// ```
pub fn single_claude() -> (Vec<BackendInfo>, BackendId) {
    let id = BackendId::trusted("claude");
    let info = BackendInfo {
        id: id.clone(),
        name: id.to_string(),
        command: "claude-agent-acp".into(),
    };
    (vec![info], id)
}

/// One command received by a [`RecordingBackend`], mirroring the [`Backend`] methods.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    /// [`Backend::new_session`].
    NewSession {
        /// The request id.
        request: RequestId,
        /// The Partitur session.
        session: SessionId,
        /// The configured backend to run it on.
        backend: BackendId,
        /// The working directory.
        cwd: PathBuf,
    },
    /// [`Backend::load_session`].
    LoadSession {
        /// The request id.
        request: RequestId,
        /// The Partitur session.
        session: SessionId,
        /// The configured backend that created it.
        backend: BackendId,
        /// The backend's id of the session to resume.
        backend_session: BackendSessionId,
        /// The working directory.
        cwd: PathBuf,
    },
    /// [`Backend::prompt`].
    Prompt {
        /// The Partitur session.
        session: SessionId,
        /// The prompt content.
        content: Vec<PromptContent>,
    },
    /// [`Backend::cancel`].
    Cancel {
        /// The Partitur session.
        session: SessionId,
    },
    /// [`Backend::set_config`].
    SetConfig {
        /// The Partitur session.
        session: SessionId,
        /// The option being set.
        option: ConfigId,
        /// The new value.
        value: ConfigValueId,
    },
    /// [`Backend::respond_permission`].
    RespondPermission {
        /// The Partitur session.
        session: SessionId,
        /// The permission request being answered.
        permission: PermissionId,
        /// The chosen option, `None` when cancelling.
        option: Option<PermissionOptionId>,
    },
}

impl Call {
    /// The request, session and working directory when this is a [`Call::NewSession`].
    pub fn as_new_session(&self) -> Option<(RequestId, &SessionId, &Path)> {
        match self {
            Self::NewSession {
                request,
                session,
                cwd,
                ..
            } => Some((*request, session, cwd)),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
struct Recording {
    calls: Vec<Call>,
    fail_next: Option<BackendError>,
}

/// A [`Backend`] that records every command instead of running it.
///
/// Clones share the same recording.
#[derive(Debug, Clone, Default)]
pub struct RecordingBackend {
    state: Arc<Mutex<Recording>>,
}

impl RecordingBackend {
    /// Creates a backend with no recorded calls.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every command received so far, oldest first, including ones that were made to fail.
    pub fn calls(&self) -> Vec<Call> {
        locked(&self.state).calls.clone()
    }

    /// Makes the next command return [`BackendError::Unavailable`]; later ones succeed again.
    pub fn fail_next_call(&self) {
        self.fail_next_call_with(BackendError::Unavailable("recording backend".into()));
    }

    /// Makes the next command return `error`; later ones succeed again.
    pub fn fail_next_call_with(&self, error: BackendError) {
        locked(&self.state).fail_next = Some(error);
    }

    fn record(&self, call: Call) -> Result<(), BackendError> {
        let mut state = locked(&self.state);
        state.calls.push(call);
        state.fail_next.take().map_or(Ok(()), Err)
    }
}

impl Backend for RecordingBackend {
    fn new_session(
        &self,
        request: RequestId,
        session: SessionId,
        backend: &BackendId,
        cwd: &Path,
    ) -> Result<(), BackendError> {
        self.record(Call::NewSession {
            request,
            session,
            backend: backend.clone(),
            cwd: cwd.to_path_buf(),
        })
    }

    fn load_session(
        &self,
        request: RequestId,
        session: SessionId,
        backend: &BackendId,
        backend_session: &BackendSessionId,
        cwd: &Path,
    ) -> Result<(), BackendError> {
        self.record(Call::LoadSession {
            request,
            session,
            backend: backend.clone(),
            backend_session: backend_session.clone(),
            cwd: cwd.to_path_buf(),
        })
    }

    fn prompt(&self, session: SessionId, content: Vec<PromptContent>) -> Result<(), BackendError> {
        self.record(Call::Prompt { session, content })
    }

    fn cancel(&self, session: SessionId) -> Result<(), BackendError> {
        self.record(Call::Cancel { session })
    }

    fn set_config(
        &self,
        session: SessionId,
        option: &ConfigId,
        value: &ConfigValueId,
    ) -> Result<(), BackendError> {
        self.record(Call::SetConfig {
            session,
            option: option.clone(),
            value: value.clone(),
        })
    }

    fn respond_permission(
        &self,
        session: SessionId,
        permission: PermissionId,
        option: Option<PermissionOptionId>,
    ) -> Result<(), BackendError> {
        self.record(Call::RespondPermission {
            session,
            permission,
            option,
        })
    }
}

#[derive(Debug, Default)]
struct Memory {
    projects: Vec<Project>,
    sessions: HashMap<ProjectId, Vec<Session>>,
    updates: HashMap<SessionId, Vec<Update>>,
    session_saves: HashMap<ProjectId, usize>,
    update_loads: usize,
    corrupt_updates: bool,
}

/// A [`Store`] that keeps everything in memory.
///
/// Clones share the same data.
#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    state: Arc<Mutex<Memory>>,
}

impl MemoryStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes [`load_updates`](Store::load_updates) report [`StoreError::Corrupt`] while `on`.
    pub fn set_corrupt_updates(&self, on: bool) {
        locked(&self.state).corrupt_updates = on;
    }

    /// How many times [`save_sessions`](Store::save_sessions) was called for `project`.
    pub fn session_saves(&self, project: &ProjectId) -> usize {
        locked(&self.state)
            .session_saves
            .get(project)
            .copied()
            .unwrap_or_default()
    }

    /// How many times [`load_updates`](Store::load_updates) was called, failed calls included.
    pub fn update_loads(&self) -> usize {
        locked(&self.state).update_loads
    }
}

impl Store for MemoryStore {
    fn load_projects(&self) -> Result<Vec<Project>, StoreError> {
        Ok(locked(&self.state).projects.clone())
    }

    fn save_projects(&self, projects: &[Project]) -> Result<(), StoreError> {
        locked(&self.state).projects = projects.to_vec();
        Ok(())
    }

    fn load_sessions(&self, project: ProjectId) -> Result<Vec<Session>, StoreError> {
        Ok(locked(&self.state)
            .sessions
            .get(&project)
            .cloned()
            .unwrap_or_default())
    }

    fn save_sessions(&self, project: ProjectId, sessions: &[Session]) -> Result<(), StoreError> {
        let mut state = locked(&self.state);
        *state.session_saves.entry(project.clone()).or_default() += 1;
        state.sessions.insert(project, sessions.to_vec());
        Ok(())
    }

    /// Like the file store, rejects a session that no saved session list contains.
    fn append_update(&self, session: SessionId, update: &Update) -> Result<(), StoreError> {
        let mut state = locked(&self.state);
        let known = state
            .sessions
            .values()
            .flatten()
            .any(|saved| saved.id == session);
        if !known {
            return Err(StoreError::Corrupt {
                path: PathBuf::from("sessions.json"),
                detail: "unknown session".into(),
            });
        }
        state
            .updates
            .entry(session)
            .or_default()
            .push(update.clone());
        Ok(())
    }

    fn load_updates(&self, session: SessionId) -> Result<Vec<Update>, StoreError> {
        let mut state = locked(&self.state);
        state.update_loads += 1;
        if state.corrupt_updates {
            return Err(StoreError::Corrupt {
                path: PathBuf::from(format!("memory/{session}.jsonl")),
                detail: "simulated corruption".into(),
            });
        }
        Ok(state.updates.get(&session).cloned().unwrap_or_default())
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

    use super::*;
    use crate::session::{Scope, Status};

    fn session(project: ProjectId) -> Session {
        Session {
            id: SessionId::new(),
            project,
            scope: Scope::Project,
            cwd: PathBuf::from("/work"),
            title: "t".into(),
            backend_session: None,
            backend: BackendId::trusted("claude"),
            model: None,
            effort: None,
            status: Status::Idle,
            parent: None,
            auto_allow: false,
            instructions_injected: false,
            created_at: 1,
        }
    }

    #[test]
    fn the_recording_backend_records_every_command_in_order() {
        let backend = RecordingBackend::new();
        let session = SessionId::new();
        let backend_session = BackendSessionId::new("b1");
        let option = ConfigId::new("model");
        let value = ConfigValueId::new("fast");
        let claude = BackendId::trusted("claude");
        let work = BackendId::trusted("work");

        backend
            .new_session(RequestId(1), session.clone(), &claude, Path::new("/a"))
            .unwrap();
        backend
            .load_session(
                RequestId(2),
                session.clone(),
                &work,
                &backend_session,
                Path::new("/b"),
            )
            .unwrap();
        backend
            .prompt(session.clone(), vec![PromptContent::Text("hi".into())])
            .unwrap();
        backend.cancel(session.clone()).unwrap();
        backend
            .set_config(session.clone(), &option, &value)
            .unwrap();
        backend
            .respond_permission(session.clone(), PermissionId(7), None)
            .unwrap();

        assert_eq!(
            backend.calls(),
            vec![
                Call::NewSession {
                    request: RequestId(1),
                    session: session.clone(),
                    backend: claude,
                    cwd: PathBuf::from("/a"),
                },
                Call::LoadSession {
                    request: RequestId(2),
                    session: session.clone(),
                    backend: work,
                    backend_session,
                    cwd: PathBuf::from("/b"),
                },
                Call::Prompt {
                    session: session.clone(),
                    content: vec![PromptContent::Text("hi".into())],
                },
                Call::Cancel {
                    session: session.clone()
                },
                Call::SetConfig {
                    session: session.clone(),
                    option,
                    value,
                },
                Call::RespondPermission {
                    session,
                    permission: PermissionId(7),
                    option: None,
                },
            ]
        );
    }

    #[test]
    fn only_a_new_session_call_converts_to_its_parts() {
        let session = SessionId::new();
        let new = Call::NewSession {
            request: RequestId(4),
            session: session.clone(),
            backend: BackendId::trusted("claude"),
            cwd: PathBuf::from("/a"),
        };

        assert_eq!(
            new.as_new_session(),
            Some((RequestId(4), &session, Path::new("/a")))
        );
        assert_eq!(Call::Cancel { session }.as_new_session(), None);
    }

    #[test]
    fn a_recording_backend_clone_shares_the_recording() {
        let backend = RecordingBackend::new();
        let clone = backend.clone();

        clone.cancel(SessionId::new()).unwrap();

        assert_eq!(backend.calls().len(), 1);
    }

    #[test]
    fn the_recording_backend_fails_exactly_the_next_call() {
        let backend = RecordingBackend::new();
        let session = SessionId::new();
        backend.fail_next_call();

        let failed = backend.cancel(session.clone());
        let retried = backend.cancel(session);

        assert!(matches!(failed, Err(BackendError::Unavailable(_))));
        assert!(retried.is_ok());
        assert_eq!(backend.calls().len(), 2);
    }

    #[test]
    fn the_recording_backend_fails_the_next_call_with_the_given_error() {
        let backend = RecordingBackend::new();
        let session = SessionId::new();
        backend.fail_next_call_with(BackendError::UnknownSession(session.clone()));

        let failed = backend.cancel(session.clone());

        assert_eq!(failed, Err(BackendError::UnknownSession(session.clone())));
        assert!(backend.cancel(session).is_ok());
    }

    #[test]
    fn the_memory_store_returns_nothing_before_anything_is_saved() {
        let store = MemoryStore::new();

        assert!(store.load_projects().unwrap().is_empty());
        assert!(store.load_sessions(ProjectId::new()).unwrap().is_empty());
        assert!(store.load_updates(SessionId::new()).unwrap().is_empty());
    }

    #[test]
    fn the_memory_store_replaces_saved_lists_and_appends_updates() {
        let store = MemoryStore::new();
        let project = Project {
            id: ProjectId::new(),
            root: PathBuf::from("/p"),
            name: "p".into(),
            repositories: Vec::new(),
            has_global_instructions: false,
            closed: false,
        };
        let mine = session(project.id.clone());
        let other = session(ProjectId::new());
        store.save_projects(std::slice::from_ref(&project)).unwrap();
        store
            .save_sessions(project.id.clone(), &[mine.clone(), mine.clone()])
            .unwrap();
        store
            .save_sessions(project.id.clone(), std::slice::from_ref(&mine))
            .unwrap();
        store
            .append_update(mine.id.clone(), &Update::UserText("a".into()))
            .unwrap();
        store
            .append_update(mine.id.clone(), &Update::AgentText("b".into()))
            .unwrap();

        assert_eq!(store.load_projects().unwrap(), vec![project.clone()]);
        assert_eq!(store.load_sessions(project.id).unwrap(), vec![mine.clone()]);
        assert!(store.load_sessions(other.project).unwrap().is_empty());
        assert_eq!(
            store.load_updates(mine.id).unwrap(),
            vec![Update::UserText("a".into()), Update::AgentText("b".into())]
        );
        assert!(store.load_updates(other.id).unwrap().is_empty());
    }

    #[test]
    fn the_memory_store_rejects_updates_for_a_session_it_never_saved() {
        let store = MemoryStore::new();
        let unknown = SessionId::new();

        let result = store.append_update(unknown.clone(), &Update::Notice("x".into()));

        assert!(matches!(
            result,
            Err(StoreError::Corrupt { ref path, ref detail })
                if *path == Path::new("sessions.json") && detail == "unknown session"
        ));
        assert!(store.load_updates(unknown).unwrap().is_empty());
    }

    #[test]
    fn the_memory_store_counts_update_loads_and_can_simulate_corruption() {
        let store = MemoryStore::new();
        let session = SessionId::new();
        assert_eq!(store.update_loads(), 0);

        store.load_updates(session.clone()).unwrap();
        store.set_corrupt_updates(true);
        let corrupt = store.load_updates(session.clone());
        store.set_corrupt_updates(false);
        let healed = store.load_updates(session);

        assert!(matches!(corrupt, Err(StoreError::Corrupt { .. })));
        assert!(healed.is_ok());
        assert_eq!(store.update_loads(), 3);
    }

    #[test]
    fn the_memory_store_counts_session_saves_per_project() {
        let store = MemoryStore::new();
        let saved = ProjectId::new();
        let untouched = ProjectId::new();

        store.save_sessions(saved.clone(), &[]).unwrap();
        store.save_sessions(saved.clone(), &[]).unwrap();

        assert_eq!(store.session_saves(&saved), 2);
        assert_eq!(store.session_saves(&untouched), 0);
    }

    #[test]
    fn a_memory_store_clone_shares_the_data() {
        let store = MemoryStore::new();
        let clone = store.clone();
        let project = ProjectId::new();

        clone
            .save_sessions(project.clone(), &[session(project.clone())])
            .unwrap();

        assert_eq!(store.load_sessions(project).unwrap().len(), 1);
    }
}
