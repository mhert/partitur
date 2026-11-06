//! [`App`], the core's single entry point for the UI.
//!
//! The app is a synchronous struct behind two ports: a [`Backend`] that runs the agents and a
//! [`Store`] that persists what the user has opened. The UI calls its methods, feeds it the
//! backend's [`BackendEvent`]s and renders the view-models it returns.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::backend::{Backend, BackendError, BackendEvent, ConfigOption, Update};
use crate::ids::{ProjectId, RepositoryId, RequestId, SessionId};
use crate::project::{Project, ProjectError, discover_project, rescan};
use crate::session::{Scope, Session, Status};
use crate::store::{Store, StoreError};
use crate::transcript::Transcript;
use crate::view_model::{ProjectNode, RepositoryNode, SessionNode, TreeModel};

/// The title of a session before its first prompt names it.
const NEW_SESSION_TITLE: &str = "New session";

/// Why an [`App`] operation failed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AppError {
    /// Persisting or loading data failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The backend could not queue a command.
    #[error(transparent)]
    Backend(#[from] BackendError),
    /// A project folder could not be discovered or rescanned.
    #[error(transparent)]
    Project(#[from] ProjectError),
    /// No open project has this id.
    #[error("unknown project {0}")]
    UnknownProject(ProjectId),
    /// No session has this id.
    #[error("unknown session {0}")]
    UnknownSession(SessionId),
    /// The project has no repository with this id.
    #[error("unknown repository {0}")]
    UnknownRepository(RepositoryId),
    /// The operation needs a selected session and none is selected.
    #[error("no session is selected")]
    NoCurrentSession,
}

/// The state of the application and the operations the UI performs on it.
///
/// Owns its [`Backend`] and [`Store`]. Backend outcomes reach it through
/// [`apply_event`](Self::apply_event).
pub struct App {
    backend: Box<dyn Backend>,
    store: Box<dyn Store>,
    /// The open projects, in the order they were opened.
    projects: Vec<Project>,
    /// The sessions of every open project, in the order they were created.
    sessions: Vec<Session>,
    /// The transcripts loaded so far; a session without an entry has not been opened yet.
    transcripts: HashMap<SessionId, Transcript>,
    /// The options each session's backend offers.
    config: HashMap<SessionId, Vec<ConfigOption>>,
    /// Backend requests that have not been answered yet, and the session each was made for.
    pending: HashMap<RequestId, SessionId>,
    next_request: u64,
    current: Option<SessionId>,
}

impl App {
    /// Restores the projects and sessions the store holds.
    ///
    /// A session cannot be running or waiting after a restart, so every session starts
    /// [`Idle`](Status::Idle). The backend is not asked to resume anything yet.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Store`] when the projects or their sessions cannot be loaded.
    pub fn load(backend: Box<dyn Backend>, store: Box<dyn Store>) -> Result<Self, AppError> {
        let projects = store.load_projects()?;
        let mut sessions = Vec::new();
        for project in &projects {
            sessions.extend(store.load_sessions(project.id.clone())?);
        }
        for session in &mut sessions {
            session.status = Status::Idle;
        }
        Ok(Self {
            backend,
            store,
            projects,
            sessions,
            transcripts: HashMap::new(),
            config: HashMap::new(),
            pending: HashMap::new(),
            next_request: 0,
            current: None,
        })
    }

    /// Opens the folder `root` as a project and saves the project list.
    ///
    /// A folder that is already open (compared by its canonical path) is rescanned instead
    /// and keeps its id.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Project`] when `root` is not a readable directory and
    /// [`AppError::Store`] when the project list cannot be saved.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use partitur_core::App;
    /// use partitur_core::testing::{MemoryStore, RecordingBackend};
    ///
    /// let dir = tempfile::tempdir()?;
    /// std::fs::create_dir_all(dir.path().join("api/.git"))?;
    /// let mut app = App::load(Box::new(RecordingBackend::new()), Box::new(MemoryStore::new()))?;
    ///
    /// let project = app.open_project(dir.path())?;
    ///
    /// assert_eq!(app.open_project(dir.path())?, project);
    /// assert_eq!(app.tree().projects[0].repositories[0].name, "api");
    /// # Ok(())
    /// # }
    /// ```
    pub fn open_project(&mut self, root: &Path) -> Result<ProjectId, AppError> {
        let canonical = fs::canonicalize(root).ok();
        let existing = canonical.and_then(|canonical| {
            self.projects
                .iter_mut()
                .find(|project| fs::canonicalize(&project.root).is_ok_and(|open| open == canonical))
        });
        if let Some(project) = existing {
            rescan(project)?;
            let id = project.id.clone();
            self.save_projects()?;
            return Ok(id);
        }
        let project = discover_project(root)?;
        let id = project.id.clone();
        self.projects.push(project);
        self.save_projects()?;
        Ok(id)
    }

    /// Closes a project and forgets its sessions in memory, then saves the project list.
    ///
    /// The sessions stay in the store; there is no way to delete them.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownProject`] when no open project has this id and
    /// [`AppError::Store`] when the project list cannot be saved.
    pub fn close_project(&mut self, project: ProjectId) -> Result<(), AppError> {
        let Some(index) = self.projects.iter().position(|open| open.id == project) else {
            return Err(AppError::UnknownProject(project));
        };
        self.projects.remove(index);
        let (closed, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.sessions)
            .into_iter()
            .partition(|session| session.project == project);
        self.sessions = kept;
        for session in closed {
            self.transcripts.remove(&session.id);
            self.config.remove(&session.id);
            self.pending.retain(|_, pending| *pending != session.id);
            if self.current.as_ref() == Some(&session.id) {
                self.current = None;
            }
        }
        self.save_projects()
    }

    /// Creates a session in `project`, asks the backend to start it and selects it.
    ///
    /// The session runs in the project root or in the repository's folder, starts
    /// [`Idle`](Status::Idle) and is titled "New session". It has no backend id until the
    /// backend answers with [`BackendEvent::SessionCreated`].
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownProject`] or [`AppError::UnknownRepository`] when `project`
    /// or the scope's repository does not exist, [`AppError::Backend`] when the backend cannot
    /// queue the command (no session is created then) and [`AppError::Store`] when the
    /// session list cannot be saved.
    pub fn new_session(&mut self, project: ProjectId, scope: Scope) -> Result<SessionId, AppError> {
        let cwd = self.working_directory(&project, &scope)?;
        let session = Session {
            id: SessionId::new(),
            project,
            scope,
            cwd,
            title: NEW_SESSION_TITLE.into(),
            backend_session: None,
            model: None,
            effort: None,
            status: Status::Idle,
            parent: None,
            auto_allow: false,
            instructions_injected: false,
            created_at: unix_seconds(),
        };
        let request = self.next_request();
        self.backend
            .new_session(request, session.id.clone(), &session.cwd)?;
        let id = session.id.clone();
        let project = session.project.clone();
        self.pending.insert(request, id.clone());
        self.transcripts.insert(id.clone(), Transcript::default());
        self.sessions.push(session);
        self.current = Some(id.clone());
        self.save_sessions(&project)?;
        Ok(id)
    }

    /// Makes `session` the current session, loading its transcript from the store the first
    /// time it is selected.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists and
    /// [`AppError::Store`] when its updates cannot be loaded; the current session is
    /// unchanged then.
    pub fn select_session(&mut self, session: SessionId) -> Result<(), AppError> {
        self.session(&session)?;
        if !self.transcripts.contains_key(&session) {
            let updates = self.store.load_updates(session.clone())?;
            self.transcripts
                .insert(session.clone(), Transcript::from_updates(&updates));
        }
        self.current = Some(session);
        Ok(())
    }

    /// The selected session, if any.
    pub fn current_session(&self) -> Option<SessionId> {
        self.current.clone()
    }

    /// Builds the navigation tree: projects in the order they were opened, each with its
    /// sessions newest first (children nested under their parent) and its repositories as
    /// discovered. Sessions created within the same second count the later one as newer.
    pub fn tree(&self) -> TreeModel {
        TreeModel {
            projects: self
                .projects
                .iter()
                .map(|project| self.project_node(project))
                .collect(),
        }
    }

    /// Applies something the backend reported.
    ///
    /// [`BackendEvent::SessionCreated`] and [`BackendEvent::SessionLoaded`] store the
    /// session's config options (the former also its backend id) and settle the request.
    /// [`BackendEvent::RequestFailed`] marks the session [`Failed`](Status::Failed) and tells
    /// the user why in a notice. [`BackendEvent::Update`] goes into the session's transcript
    /// and the store.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when the event names a session that does not
    /// exist and [`AppError::Store`] when the change cannot be saved.
    pub fn apply_event(&mut self, event: BackendEvent) -> Result<(), AppError> {
        match event {
            BackendEvent::SessionCreated {
                request,
                session,
                backend_session,
                config,
            } => {
                let created = self.session_mut(&session)?;
                created.backend_session = Some(backend_session);
                let project = created.project.clone();
                self.settle_request(request, session, config);
                self.save_sessions(&project)
            }
            BackendEvent::SessionLoaded {
                request,
                session,
                config,
            } => {
                self.session(&session)?;
                self.settle_request(request, session, config);
                Ok(())
            }
            BackendEvent::Update { session, update } => self.apply_update(&session, &update),
            BackendEvent::RequestFailed {
                request,
                session,
                error,
            } => {
                let failed = self.session_mut(&session)?;
                failed.status = Status::Failed;
                let project = failed.project.clone();
                self.pending.remove(&request);
                self.save_sessions(&project)?;
                self.apply_update(&session, &Update::Notice(error))
            }
        }
    }

    fn project(&self, id: &ProjectId) -> Result<&Project, AppError> {
        self.projects
            .iter()
            .find(|project| &project.id == id)
            .ok_or_else(|| AppError::UnknownProject(id.clone()))
    }

    fn session(&self, id: &SessionId) -> Result<&Session, AppError> {
        self.sessions
            .iter()
            .find(|session| &session.id == id)
            .ok_or_else(|| AppError::UnknownSession(id.clone()))
    }

    fn session_mut(&mut self, id: &SessionId) -> Result<&mut Session, AppError> {
        self.sessions
            .iter_mut()
            .find(|session| &session.id == id)
            .ok_or_else(|| AppError::UnknownSession(id.clone()))
    }

    fn working_directory(&self, project: &ProjectId, scope: &Scope) -> Result<PathBuf, AppError> {
        let project = self.project(project)?;
        match scope {
            Scope::Project => Ok(project.root.clone()),
            Scope::Repository(id) => project
                .repositories
                .iter()
                .find(|repository| &repository.id == id)
                .map(|repository| repository.path.clone())
                .ok_or_else(|| AppError::UnknownRepository(id.clone())),
        }
    }

    fn next_request(&mut self) -> RequestId {
        self.next_request = self.next_request.saturating_add(1);
        RequestId(self.next_request)
    }

    fn settle_request(
        &mut self,
        request: RequestId,
        session: SessionId,
        config: Vec<ConfigOption>,
    ) {
        self.pending.remove(&request);
        self.config.insert(session, config);
    }

    /// Applies `update` to the session's transcript when it is loaded and appends it to the
    /// store; an unloaded transcript picks it up from the store when it is selected.
    fn apply_update(&mut self, session: &SessionId, update: &Update) -> Result<(), AppError> {
        self.session(session)?;
        if let Some(transcript) = self.transcripts.get_mut(session) {
            transcript.apply(update);
        }
        self.store.append_update(session.clone(), update)?;
        Ok(())
    }

    fn save_projects(&self) -> Result<(), AppError> {
        self.store.save_projects(&self.projects)?;
        Ok(())
    }

    fn save_sessions(&self, project: &ProjectId) -> Result<(), AppError> {
        let sessions: Vec<Session> = self
            .sessions
            .iter()
            .filter(|session| &session.project == project)
            .cloned()
            .collect();
        self.store.save_sessions(project.clone(), &sessions)?;
        Ok(())
    }

    fn project_node(&self, project: &Project) -> ProjectNode {
        ProjectNode {
            id: project.id.clone(),
            name: project.name.clone(),
            root: project.root.clone(),
            has_global_instructions: project.has_global_instructions,
            sessions: self.session_nodes(project, None),
            repositories: project
                .repositories
                .iter()
                .map(|repository| RepositoryNode {
                    id: repository.id.clone(),
                    name: repository.name.clone(),
                    has_instructions: repository.has_instructions,
                })
                .collect(),
        }
    }

    /// The sessions of `project` whose parent is `parent`, newest first.
    fn session_nodes(&self, project: &Project, parent: Option<&SessionId>) -> Vec<SessionNode> {
        // Walking the sessions backwards puts later-created ones first, which the stable
        // sort keeps for equal timestamps.
        let mut siblings: Vec<&Session> = self
            .sessions
            .iter()
            .rev()
            .filter(|session| session.project == project.id && session.parent.as_ref() == parent)
            .collect();
        siblings.sort_by_key(|session| Reverse(session.created_at));
        siblings
            .into_iter()
            .map(|session| SessionNode {
                id: session.id.clone(),
                title: session.title.clone(),
                status: session.status,
                scope_label: scope_label(project, &session.scope),
                children: self.session_nodes(project, Some(&session.id)),
            })
            .collect()
    }
}

/// The name of what a session works on; a repository that is gone falls back to its id.
fn scope_label(project: &Project, scope: &Scope) -> String {
    match scope {
        Scope::Project => project.name.clone(),
        Scope::Repository(id) => project
            .repositories
            .iter()
            .find(|repository| &repository.id == id)
            .map_or_else(|| id.to_string(), |repository| repository.name.clone()),
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use tempfile::TempDir;

    use super::*;
    use crate::backend::{ConfigCategory, ConfigChoice};
    use crate::ids::{BackendSessionId, ConfigId, ConfigValueId};
    use crate::session::title_from_prompt;
    use crate::testing::{MemoryStore, RecordingBackend};
    use crate::transcript::Entry;

    struct Fixture {
        app: App,
        backend: RecordingBackend,
        store: MemoryStore,
    }

    fn fixture() -> Fixture {
        fixture_with(MemoryStore::new())
    }

    fn fixture_with(store: MemoryStore) -> Fixture {
        let backend = RecordingBackend::new();
        let app = App::load(Box::new(backend.clone()), Box::new(store.clone())).unwrap();
        Fixture {
            app,
            backend,
            store,
        }
    }

    /// A project folder whose children named in `repositories` are git repositories.
    fn project_dir(repositories: &[&str]) -> TempDir {
        let dir = TempDir::new().unwrap();
        for name in repositories {
            fs::create_dir_all(dir.path().join(name).join(".git")).unwrap();
        }
        dir
    }

    fn stored_session(project: &Project, title: &str, created_at: u64) -> Session {
        Session {
            id: SessionId::new(),
            project: project.id.clone(),
            scope: Scope::Project,
            cwd: project.root.clone(),
            title: title.into(),
            backend_session: Some(BackendSessionId::new(format!("backend-{title}"))),
            model: None,
            effort: None,
            status: Status::Idle,
            parent: None,
            auto_allow: false,
            instructions_injected: false,
            created_at,
        }
    }

    fn model_option() -> ConfigOption {
        ConfigOption {
            id: ConfigId::new("model"),
            name: "Model".into(),
            category: ConfigCategory::Model,
            current: ConfigValueId::new("fast"),
            choices: vec![ConfigChoice {
                id: ConfigValueId::new("fast"),
                name: "Fast".into(),
            }],
        }
    }

    fn new_session_request(backend: &RecordingBackend) -> (RequestId, SessionId, PathBuf) {
        let calls = backend.calls();
        let (request, session, cwd) = calls.last().unwrap().as_new_session().unwrap();
        (request, session.clone(), cwd.to_path_buf())
    }

    fn titles(nodes: &[SessionNode]) -> Vec<&str> {
        nodes.iter().map(|node| node.title.as_str()).collect()
    }

    fn session_status(app: &App, id: &SessionId) -> Status {
        app.sessions.iter().find(|s| &s.id == id).unwrap().status
    }

    #[test]
    fn load_restores_projects_and_sessions_and_resets_live_statuses_to_idle() {
        let dir = project_dir(&["api"]);
        let project = discover_project(dir.path()).unwrap();
        let mut running = stored_session(&project, "running", 1);
        running.status = Status::Running;
        let mut waiting = stored_session(&project, "waiting", 2);
        waiting.status = Status::WaitingForYou;
        let mut done = stored_session(&project, "done", 3);
        done.status = Status::Done;
        let mut failed = stored_session(&project, "failed", 4);
        failed.status = Status::Failed;
        let store = MemoryStore::new();
        store.save_projects(std::slice::from_ref(&project)).unwrap();
        store
            .save_sessions(
                project.id.clone(),
                &[running.clone(), waiting, done, failed],
            )
            .unwrap();

        let fixture = fixture_with(store);

        let tree = fixture.app.tree();
        assert_eq!(tree.projects.len(), 1);
        assert_eq!(tree.projects[0].id, project.id);
        assert_eq!(tree.projects[0].sessions.len(), 4);
        assert!(
            tree.projects[0]
                .sessions
                .iter()
                .all(|node| node.status == Status::Idle)
        );
        let restored = fixture
            .app
            .sessions
            .iter()
            .find(|s| s.id == running.id)
            .unwrap();
        assert_eq!(restored.backend_session, running.backend_session);
        assert!(fixture.backend.calls().is_empty());
        assert_eq!(fixture.app.current_session(), None);
    }

    #[test]
    fn opening_a_project_twice_returns_the_same_id_after_a_rescan() {
        let dir = project_dir(&[]);
        let mut fixture = fixture();
        let first = fixture.app.open_project(dir.path()).unwrap();
        fs::create_dir_all(dir.path().join("api/.git")).unwrap();

        let second = fixture.app.open_project(&dir.path().join(".")).unwrap();

        assert_eq!(first, second);
        let tree = fixture.app.tree();
        assert_eq!(tree.projects.len(), 1);
        assert_eq!(tree.projects[0].repositories.len(), 1);
        assert_eq!(fixture.store.load_projects().unwrap().len(), 1);
        assert_eq!(
            fixture.store.load_projects().unwrap()[0].repositories.len(),
            1
        );
    }

    #[test]
    fn opening_projects_keeps_open_order_and_saves_them() {
        let first_dir = project_dir(&[]);
        let second_dir = project_dir(&[]);
        let mut fixture = fixture();

        let first = fixture.app.open_project(first_dir.path()).unwrap();
        let second = fixture.app.open_project(second_dir.path()).unwrap();

        assert_ne!(first, second);
        let ids: Vec<_> = fixture
            .app
            .tree()
            .projects
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, vec![first.clone(), second.clone()]);
        let saved: Vec<_> = fixture
            .store
            .load_projects()
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(saved, vec![first, second]);
    }

    #[test]
    fn opening_something_that_is_not_a_directory_fails() {
        let dir = project_dir(&[]);
        let mut fixture = fixture();

        let result = fixture.app.open_project(&dir.path().join("missing"));

        assert!(matches!(
            result,
            Err(AppError::Project(ProjectError::NotADirectory { .. }))
        ));
        assert!(fixture.app.tree().projects.is_empty());
        assert!(fixture.store.load_projects().unwrap().is_empty());
    }

    #[test]
    fn a_new_project_session_runs_in_the_root_and_a_repository_session_in_the_repository() {
        let dir = project_dir(&["api"]);
        let mut fixture = fixture();
        let project = fixture.app.open_project(dir.path()).unwrap();
        let repository = RepositoryId::try_from("api").unwrap();

        let in_project = fixture
            .app
            .new_session(project.clone(), Scope::Project)
            .unwrap();
        let (_, _, project_cwd) = new_session_request(&fixture.backend);
        let in_repository = fixture
            .app
            .new_session(project, Scope::Repository(repository.clone()))
            .unwrap();
        let (_, _, repository_cwd) = new_session_request(&fixture.backend);

        assert_eq!(project_cwd, dir.path());
        assert_eq!(repository_cwd, dir.path().join("api"));
        let stored = fixture.app.sessions.clone();
        let project_session = stored.iter().find(|s| s.id == in_project).unwrap();
        let repository_session = stored.iter().find(|s| s.id == in_repository).unwrap();
        assert_eq!(project_session.scope, Scope::Project);
        assert_eq!(project_session.cwd, dir.path());
        assert_eq!(repository_session.scope, Scope::Repository(repository));
        assert_eq!(repository_session.cwd, dir.path().join("api"));
    }

    #[test]
    fn new_session_asks_the_backend_for_a_session_and_becomes_current() {
        let dir = project_dir(&[]);
        let mut fixture = fixture();
        let project = fixture.app.open_project(dir.path()).unwrap();

        let first = fixture
            .app
            .new_session(project.clone(), Scope::Project)
            .unwrap();
        let (first_request, requested, cwd) = new_session_request(&fixture.backend);
        let second = fixture
            .app
            .new_session(project.clone(), Scope::Project)
            .unwrap();
        let (second_request, _, _) = new_session_request(&fixture.backend);

        assert_eq!(requested, first);
        assert_eq!(cwd, dir.path());
        assert_ne!(first_request, second_request);
        assert_eq!(fixture.backend.calls().len(), 2);
        assert_eq!(fixture.app.current_session(), Some(second.clone()));
        let session = fixture.app.sessions.iter().find(|s| s.id == first).unwrap();
        assert_eq!(session.title, "New session");
        assert_eq!(session.status, Status::Idle);
        assert_eq!(session.project, project);
        assert_eq!(session.backend_session, None);
        assert_eq!(session.parent, None);
        assert!(!session.auto_allow);
        assert!(!session.instructions_injected);
        assert!(session.created_at > 1_700_000_000);
        assert_eq!(
            fixture.store.load_sessions(project).unwrap(),
            fixture.app.sessions
        );
        assert!(fixture.app.pending.contains_key(&first_request));
        assert!(fixture.app.transcripts.contains_key(&first));
    }

    #[test]
    fn new_session_rejects_unknown_projects_and_repositories() {
        let dir = project_dir(&["api"]);
        let mut fixture = fixture();
        let project = fixture.app.open_project(dir.path()).unwrap();
        let unknown_project = ProjectId::new();
        let unknown_repository = RepositoryId::try_from("ghost").unwrap();

        let no_project = fixture
            .app
            .new_session(unknown_project.clone(), Scope::Project);
        let no_repository = fixture
            .app
            .new_session(project, Scope::Repository(unknown_repository.clone()));

        assert!(matches!(no_project, Err(AppError::UnknownProject(id)) if id == unknown_project));
        assert!(
            matches!(no_repository, Err(AppError::UnknownRepository(id)) if id == unknown_repository)
        );
        assert!(fixture.backend.calls().is_empty());
        assert!(fixture.app.sessions.is_empty());
    }

    #[test]
    fn a_backend_that_cannot_queue_the_create_leaves_no_session_behind() {
        let dir = project_dir(&[]);
        let mut fixture = fixture();
        let project = fixture.app.open_project(dir.path()).unwrap();
        let before = fixture
            .app
            .new_session(project.clone(), Scope::Project)
            .unwrap();
        fixture.backend.fail_next_call();

        let result = fixture.app.new_session(project.clone(), Scope::Project);

        assert!(matches!(
            result,
            Err(AppError::Backend(BackendError::Unavailable(_)))
        ));
        assert_eq!(fixture.app.current_session(), Some(before));
        assert_eq!(fixture.app.sessions.len(), 1);
        assert_eq!(fixture.app.pending.len(), 1);
        assert_eq!(fixture.store.load_sessions(project).unwrap().len(), 1);
    }

    #[test]
    fn session_created_records_the_backend_id_and_config_options() {
        let dir = project_dir(&[]);
        let mut fixture = fixture();
        let project = fixture.app.open_project(dir.path()).unwrap();
        let session = fixture
            .app
            .new_session(project.clone(), Scope::Project)
            .unwrap();
        let (request, _, _) = new_session_request(&fixture.backend);

        fixture
            .app
            .apply_event(BackendEvent::SessionCreated {
                request,
                session: session.clone(),
                backend_session: BackendSessionId::new("b-1"),
                config: vec![model_option()],
            })
            .unwrap();

        let stored = fixture.store.load_sessions(project).unwrap();
        assert_eq!(
            stored[0].backend_session,
            Some(BackendSessionId::new("b-1"))
        );
        assert_eq!(
            fixture.app.sessions[0].backend_session,
            Some(BackendSessionId::new("b-1"))
        );
        assert_eq!(fixture.app.config[&session], vec![model_option()]);
        assert!(fixture.app.pending.is_empty());
    }

    #[test]
    fn session_loaded_remembers_the_config_options_and_clears_the_pending_request() {
        let dir = project_dir(&[]);
        let mut fixture = fixture();
        let project = fixture.app.open_project(dir.path()).unwrap();
        let session = fixture.app.new_session(project, Scope::Project).unwrap();
        let backend_session = BackendSessionId::new("kept");
        fixture.app.sessions[0].backend_session = Some(backend_session.clone());
        fixture.app.pending.clear();
        fixture.app.pending.insert(RequestId(99), session.clone());

        fixture
            .app
            .apply_event(BackendEvent::SessionLoaded {
                request: RequestId(99),
                session: session.clone(),
                config: vec![model_option()],
            })
            .unwrap();

        assert_eq!(fixture.app.config[&session], vec![model_option()]);
        assert!(fixture.app.pending.is_empty());
        assert_eq!(
            fixture.app.sessions[0].backend_session,
            Some(backend_session)
        );
    }

    #[test]
    fn events_for_unknown_sessions_are_rejected() {
        let mut fixture = fixture();
        let ghost = SessionId::new();
        let events = [
            BackendEvent::SessionCreated {
                request: RequestId(1),
                session: ghost.clone(),
                backend_session: BackendSessionId::new("b"),
                config: Vec::new(),
            },
            BackendEvent::SessionLoaded {
                request: RequestId(1),
                session: ghost.clone(),
                config: Vec::new(),
            },
            BackendEvent::Update {
                session: ghost.clone(),
                update: Update::UserText("hi".into()),
            },
            BackendEvent::RequestFailed {
                request: RequestId(1),
                session: ghost.clone(),
                error: "boom".into(),
            },
        ];

        for event in events {
            let result = fixture.app.apply_event(event);
            assert!(matches!(result, Err(AppError::UnknownSession(id)) if id == ghost));
        }

        assert!(fixture.app.config.is_empty());
        assert!(fixture.store.load_updates(ghost).unwrap().is_empty());
    }

    #[test]
    fn a_failed_create_marks_the_session_failed_with_a_notice() {
        let dir = project_dir(&[]);
        let mut fixture = fixture();
        let project = fixture.app.open_project(dir.path()).unwrap();
        let session = fixture
            .app
            .new_session(project.clone(), Scope::Project)
            .unwrap();
        let (request, _, _) = new_session_request(&fixture.backend);

        fixture
            .app
            .apply_event(BackendEvent::RequestFailed {
                request,
                session: session.clone(),
                error: "agent did not start".into(),
            })
            .unwrap();

        assert_eq!(session_status(&fixture.app, &session), Status::Failed);
        assert_eq!(
            fixture.store.load_sessions(project).unwrap()[0].status,
            Status::Failed
        );
        assert_eq!(
            fixture.app.transcripts[&session].entries(),
            [Entry::Notice("agent did not start".into())]
        );
        assert_eq!(
            fixture.store.load_updates(session).unwrap(),
            vec![Update::Notice("agent did not start".into())]
        );
        assert!(fixture.app.pending.is_empty());
    }

    #[test]
    fn updates_are_persisted_and_applied_to_a_loaded_transcript() {
        let dir = project_dir(&[]);
        let mut fixture = fixture();
        let project = fixture.app.open_project(dir.path()).unwrap();
        let session = fixture.app.new_session(project, Scope::Project).unwrap();

        fixture
            .app
            .apply_event(BackendEvent::Update {
                session: session.clone(),
                update: Update::AgentText("hello".into()),
            })
            .unwrap();

        assert_eq!(
            fixture.app.transcripts[&session].entries(),
            [Entry::Agent("hello".into())]
        );
        assert_eq!(
            fixture.store.load_updates(session).unwrap(),
            vec![Update::AgentText("hello".into())]
        );
    }

    #[test]
    fn updates_for_a_session_not_yet_opened_are_only_persisted() {
        let dir = project_dir(&[]);
        let project = discover_project(dir.path()).unwrap();
        let stored = stored_session(&project, "old", 1);
        let store = MemoryStore::new();
        store.save_projects(std::slice::from_ref(&project)).unwrap();
        store
            .save_sessions(project.id, std::slice::from_ref(&stored))
            .unwrap();
        let mut fixture = fixture_with(store);

        fixture
            .app
            .apply_event(BackendEvent::Update {
                session: stored.id.clone(),
                update: Update::AgentText("late".into()),
            })
            .unwrap();
        assert!(!fixture.app.transcripts.contains_key(&stored.id));
        fixture.app.select_session(stored.id.clone()).unwrap();

        assert_eq!(
            fixture.app.transcripts[&stored.id].entries(),
            [Entry::Agent("late".into())]
        );
    }

    #[test]
    fn the_tree_lists_sessions_newest_first_under_their_project() {
        let dir = project_dir(&["api", "web"]);
        let other_dir = project_dir(&[]);
        let project = discover_project(dir.path()).unwrap();
        let other = discover_project(other_dir.path()).unwrap();
        let oldest = stored_session(&project, "oldest", 10);
        let newest = stored_session(&project, "newest", 30);
        let middle = stored_session(&project, "middle", 20);
        let tie_first = stored_session(&project, "tie-first", 30);
        let mut api = stored_session(&project, "api", 5);
        api.scope = Scope::Repository(RepositoryId::try_from("api").unwrap());
        let elsewhere = stored_session(&other, "elsewhere", 99);
        let store = MemoryStore::new();
        store
            .save_projects(&[project.clone(), other.clone()])
            .unwrap();
        store
            .save_sessions(
                project.id.clone(),
                &[oldest, newest, middle, tie_first, api],
            )
            .unwrap();
        store.save_sessions(other.id.clone(), &[elsewhere]).unwrap();

        let tree = fixture_with(store).app.tree();

        let first = &tree.projects[0];
        assert_eq!(first.name, project.name);
        assert_eq!(first.root, project.root);
        assert!(!first.has_global_instructions);
        assert_eq!(
            titles(&first.sessions),
            ["tie-first", "newest", "middle", "oldest", "api"]
        );
        assert_eq!(first.sessions[0].scope_label, project.name);
        assert_eq!(first.sessions[4].scope_label, "api");
        assert_eq!(first.repositories.len(), 2);
        assert_eq!(first.repositories[0].name, "api");
        assert_eq!(
            first.repositories[0].id,
            RepositoryId::try_from("api").unwrap()
        );
        assert!(!first.repositories[0].has_instructions);
        assert_eq!(titles(&tree.projects[1].sessions), ["elsewhere"]);
    }

    #[test]
    fn the_tree_flags_instruction_files_and_nests_children_under_their_parent() {
        let dir = project_dir(&["api"]);
        fs::write(dir.path().join("AGENTS.md"), "rules").unwrap();
        fs::write(dir.path().join("api/AGENTS.md"), "rules").unwrap();
        let project = discover_project(dir.path()).unwrap();
        let parent = stored_session(&project, "parent", 10);
        let mut older_child = stored_session(&project, "older child", 11);
        older_child.parent = Some(parent.id.clone());
        let mut newer_child = stored_session(&project, "newer child", 12);
        newer_child.parent = Some(parent.id.clone());
        let mut grandchild = stored_session(&project, "grandchild", 13);
        grandchild.parent = Some(newer_child.id.clone());
        let store = MemoryStore::new();
        store.save_projects(std::slice::from_ref(&project)).unwrap();
        store
            .save_sessions(project.id, &[parent, older_child, newer_child, grandchild])
            .unwrap();

        let tree = fixture_with(store).app.tree();

        let node = &tree.projects[0];
        assert!(node.has_global_instructions);
        assert!(node.repositories[0].has_instructions);
        assert_eq!(titles(&node.sessions), ["parent"]);
        let children = &node.sessions[0].children;
        assert_eq!(titles(children), ["newer child", "older child"]);
        assert_eq!(titles(&children[0].children), ["grandchild"]);
        assert!(children[1].children.is_empty());
    }

    #[test]
    fn a_session_of_a_vanished_repository_is_labelled_with_the_repository_id() {
        let dir = project_dir(&["api"]);
        let mut fixture = fixture();
        let project = fixture.app.open_project(dir.path()).unwrap();
        fixture
            .app
            .new_session(
                project.clone(),
                Scope::Repository(RepositoryId::try_from("api").unwrap()),
            )
            .unwrap();
        fs::remove_dir_all(dir.path().join("api/.git")).unwrap();
        fixture.app.open_project(dir.path()).unwrap();

        let tree = fixture.app.tree();

        assert!(tree.projects[0].repositories.is_empty());
        assert_eq!(tree.projects[0].sessions[0].scope_label, "api");
    }

    #[test]
    fn selecting_a_session_loads_its_transcript_once() {
        let dir = project_dir(&[]);
        let project = discover_project(dir.path()).unwrap();
        let first = stored_session(&project, "first", 1);
        let second = stored_session(&project, "second", 2);
        let store = MemoryStore::new();
        store.save_projects(std::slice::from_ref(&project)).unwrap();
        store
            .save_sessions(project.id, &[first.clone(), second.clone()])
            .unwrap();
        store
            .append_update(first.id.clone(), &Update::UserText("hi".into()))
            .unwrap();
        let mut fixture = fixture_with(store);

        fixture.app.select_session(first.id.clone()).unwrap();
        fixture.app.select_session(second.id.clone()).unwrap();
        fixture.app.select_session(first.id.clone()).unwrap();

        assert_eq!(fixture.store.update_loads(), 2);
        assert_eq!(fixture.app.current_session(), Some(first.id.clone()));
        assert_eq!(
            fixture.app.transcripts[&first.id].entries(),
            [Entry::User("hi".into())]
        );
        assert!(fixture.app.transcripts[&second.id].entries().is_empty());
    }

    #[test]
    fn selecting_an_unknown_session_or_one_with_a_corrupt_log_keeps_the_current_session() {
        let dir = project_dir(&[]);
        let project = discover_project(dir.path()).unwrap();
        let good = stored_session(&project, "good", 1);
        let bad = stored_session(&project, "bad", 2);
        let store = MemoryStore::new();
        store.save_projects(std::slice::from_ref(&project)).unwrap();
        store
            .save_sessions(project.id, &[good.clone(), bad.clone()])
            .unwrap();
        let mut fixture = fixture_with(store);
        fixture.app.select_session(good.id.clone()).unwrap();
        fixture.store.set_corrupt_updates(true);

        let unknown = fixture.app.select_session(SessionId::new());
        let corrupt = fixture.app.select_session(bad.id.clone());

        assert!(matches!(unknown, Err(AppError::UnknownSession(_))));
        assert!(matches!(
            corrupt,
            Err(AppError::Store(StoreError::Corrupt { .. }))
        ));
        assert_eq!(fixture.app.current_session(), Some(good.id));
        assert!(!fixture.app.transcripts.contains_key(&bad.id));
    }

    #[test]
    fn closing_a_project_forgets_its_sessions_but_keeps_the_others() {
        let dir = project_dir(&[]);
        let other_dir = project_dir(&[]);
        let mut fixture = fixture();
        let closed = fixture.app.open_project(dir.path()).unwrap();
        let kept = fixture.app.open_project(other_dir.path()).unwrap();
        let closed_session = fixture
            .app
            .new_session(closed.clone(), Scope::Project)
            .unwrap();
        let kept_session = fixture
            .app
            .new_session(kept.clone(), Scope::Project)
            .unwrap();
        fixture.app.select_session(closed_session.clone()).unwrap();
        fixture
            .app
            .config
            .insert(closed_session.clone(), vec![model_option()]);
        fixture
            .app
            .config
            .insert(kept_session.clone(), vec![model_option()]);

        fixture.app.close_project(closed.clone()).unwrap();

        let tree = fixture.app.tree();
        assert_eq!(tree.projects.len(), 1);
        assert_eq!(tree.projects[0].id, kept);
        assert_eq!(tree.projects[0].sessions.len(), 1);
        assert_eq!(fixture.app.current_session(), None);
        assert!(!fixture.app.transcripts.contains_key(&closed_session));
        assert!(fixture.app.transcripts.contains_key(&kept_session));
        assert!(!fixture.app.config.contains_key(&closed_session));
        assert!(fixture.app.config.contains_key(&kept_session));
        assert_eq!(
            fixture.app.pending.values().collect::<Vec<_>>(),
            [&kept_session]
        );
        let saved = fixture.store.load_projects().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].id, kept);
        assert_eq!(fixture.store.load_sessions(closed).unwrap().len(), 1);
        assert_eq!(fixture.store.load_sessions(kept).unwrap().len(), 1);
    }

    #[test]
    fn closing_a_project_keeps_the_current_session_of_another_project() {
        let dir = project_dir(&[]);
        let other_dir = project_dir(&[]);
        let mut fixture = fixture();
        let closed = fixture.app.open_project(dir.path()).unwrap();
        let kept = fixture.app.open_project(other_dir.path()).unwrap();
        fixture
            .app
            .new_session(closed.clone(), Scope::Project)
            .unwrap();
        let kept_session = fixture.app.new_session(kept, Scope::Project).unwrap();

        fixture.app.close_project(closed).unwrap();

        assert_eq!(fixture.app.current_session(), Some(kept_session));
    }

    #[test]
    fn closing_an_unknown_project_fails() {
        let mut fixture = fixture();
        let ghost = ProjectId::new();

        let result = fixture.app.close_project(ghost.clone());

        assert!(matches!(result, Err(AppError::UnknownProject(id)) if id == ghost));
    }

    #[test]
    fn title_from_prompt_takes_the_first_line_and_cuts_at_sixty() {
        let sixty = "a".repeat(60);
        let long = "é".repeat(61);

        assert_eq!(title_from_prompt("Fix the build"), "Fix the build");
        assert_eq!(
            title_from_prompt("\n \t\n  Fix it  \nthen deploy"),
            "Fix it"
        );
        assert_eq!(title_from_prompt(&sixty), sixty);
        assert_eq!(title_from_prompt(&long), format!("{}…", "é".repeat(60)));
        assert_eq!(title_from_prompt(""), "");
        assert_eq!(title_from_prompt(" \n \n"), "");
    }
}
