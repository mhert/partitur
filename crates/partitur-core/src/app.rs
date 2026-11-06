//! [`App`], the core's single entry point for the UI.
//!
//! The app is a synchronous struct behind two ports: a [`Backend`] that runs the agents and a
//! [`Store`] that persists what the user has opened. The UI calls its methods, feeds it the
//! backend's [`BackendEvent`]s and renders the view-models it returns.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use thiserror::Error;

use crate::backend::{
    Backend, BackendError, BackendEvent, ConfigCategory, ConfigOption, PermissionKind,
    PermissionOption, PromptContent, Update,
};
use crate::ids::{
    ConfigValueId, PermissionId, PermissionOptionId, ProjectId, RepositoryId, RequestId, SessionId,
};
use crate::instructions::{InstructionChain, SkippedInstruction, inject, instruction_chain};
use crate::project::{Project, ProjectError, discover_project, rescan};
use crate::session::{Scope, Session, Status, title_from_prompt};
use crate::store::{Store, StoreError};
use crate::transcript::Transcript;
use crate::view_model::{EnvViewModel, SessionViewModel, TreeModel};

/// The title of a session before its first prompt names it.
const NEW_SESSION_TITLE: &str = "New session";

/// The notice shown when a stored session could not be resumed and a fresh one replaced it.
const RESUMED_NOTICE: &str = "resumed without the agent's memory of earlier turns";

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
    /// An attached image could not be read, so the prompt was not sent.
    #[error("cannot read attachment {}: {source}", path.display())]
    Attachment {
        /// The attached file.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// The session's backend offers no configuration option of this category.
    #[error("the session offers no {0:?} option")]
    NoSuchConfig(ConfigCategory),
}

/// A file the user attached to a prompt.
///
/// PNG, JPEG, GIF and WebP images (by extension, in any case) are read and sent inline; any
/// other file is sent as a link the agent can read itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// The attached file.
    pub path: PathBuf,
}

/// What an unanswered backend request asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Request {
    /// [`Backend::new_session`].
    Create,
    /// [`Backend::load_session`].
    Load,
}

/// A prompt waiting for its session's backend session to be created or loaded.
#[derive(Debug)]
struct QueuedPrompt {
    /// The user's text, without instructions.
    text: String,
    /// The attachments, already converted.
    attachments: Vec<PromptContent>,
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
    /// Backend requests that have not been answered yet: the session each was made for and
    /// what it asked for.
    pending: HashMap<RequestId, (SessionId, Request)>,
    /// Sessions whose backend session was created or loaded by this process, so prompts can
    /// go to it directly. A backend id restored from the store is not attached.
    attached: HashSet<SessionId>,
    /// Prompts waiting for their session to be attached, oldest first.
    queued: HashMap<SessionId, Vec<QueuedPrompt>>,
    /// Permission requests waiting for the user's answer.
    permissions: HashSet<(SessionId, PermissionId)>,
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
            attached: HashSet::new(),
            queued: HashMap::new(),
            permissions: HashSet::new(),
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
            self.pending
                .retain(|_, (pending, _)| *pending != session.id);
            self.attached.remove(&session.id);
            self.queued.remove(&session.id);
            self.permissions
                .retain(|(waiting, _)| *waiting != session.id);
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
        self.pending.insert(request, (id.clone(), Request::Create));
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
        TreeModel::build(&self.projects, &self.sessions)
    }

    /// Sends a prompt to the current session.
    ///
    /// Text that is blank with no attachments does nothing. The first prompt names an untitled
    /// session. The prompt that goes to the backend carries the session's instruction chain
    /// (see [`inject`]) as long as the session has not received it; each instruction file that
    /// cannot be read is reported in a notice. The transcript records the text as typed.
    ///
    /// A session whose backend session is not attached yet (still being created, never
    /// created, or restored from the store) queues the prompt and asks the backend to create
    /// or load it; the prompt goes out once that succeeds. Either way the session becomes
    /// [`Running`](Status::Running).
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoCurrentSession`] when no session is selected,
    /// [`AppError::Attachment`] when an attached image cannot be read,
    /// [`AppError::Backend`] when the backend cannot queue the command (nothing changes in
    /// these cases) and [`AppError::Store`] when the change cannot be saved.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use partitur_core::testing::{Call, MemoryStore, RecordingBackend};
    /// use partitur_core::{App, BackendEvent, BackendSessionId, PromptContent, Scope};
    ///
    /// let dir = tempfile::tempdir()?;
    /// let backend = RecordingBackend::new();
    /// let mut app = App::load(Box::new(backend.clone()), Box::new(MemoryStore::new()))?;
    /// let project = app.open_project(dir.path())?;
    /// let session = app.new_session(project, Scope::Project)?;
    /// let calls = backend.calls();
    /// let (request, _, _) = calls.first().and_then(Call::as_new_session).ok_or("no create")?;
    ///
    /// // The backend has not created the session yet, so the prompt waits for it.
    /// app.submit_prompt("Fix the build", Vec::new())?;
    /// app.apply_event(BackendEvent::SessionCreated {
    ///     request,
    ///     session: session.clone(),
    ///     backend_session: BackendSessionId::new("b-1"),
    ///     config: Vec::new(),
    /// })?;
    ///
    /// let prompt = Call::Prompt {
    ///     session,
    ///     content: vec![PromptContent::Text("Fix the build".into())],
    /// };
    /// assert_eq!(backend.calls().last(), Some(&prompt));
    /// let view = app.session_view().ok_or("no session")?;
    /// assert_eq!(view.breadcrumb.last().map(String::as_str), Some("Fix the build"));
    /// assert!(view.running);
    /// # Ok(())
    /// # }
    /// ```
    pub fn submit_prompt(
        &mut self,
        text: &str,
        attachments: Vec<Attachment>,
    ) -> Result<(), AppError> {
        let id = self.current.clone().ok_or(AppError::NoCurrentSession)?;
        if text.trim().is_empty() && attachments.is_empty() {
            return Ok(());
        }
        let prompt = QueuedPrompt {
            text: text.to_owned(),
            attachments: attachments
                .into_iter()
                .map(prompt_content)
                .collect::<Result<_, _>>()?,
        };
        let skipped = if self.attached.contains(&id) {
            self.send_prompt(&id, prompt)?
        } else {
            self.request_backend_session(&id)?;
            self.queued.entry(id.clone()).or_default().push(prompt);
            Vec::new()
        };
        self.apply_update(&id, &Update::UserText(text.to_owned()))?;
        self.report_skipped(&id, skipped)?;
        self.change_session(id, |session| {
            if session.title == NEW_SESSION_TITLE {
                let title = title_from_prompt(text);
                if !title.is_empty() {
                    session.title = title;
                }
            }
            session.status = Status::Running;
        })
    }

    /// Stops the current session's turn.
    ///
    /// A running turn is cancelled by the backend and the session stays
    /// [`Running`](Status::Running) until the turn's end arrives. A prompt still waiting for
    /// its backend session is dropped instead and the session becomes [`Idle`](Status::Idle).
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoCurrentSession`] when no session is selected,
    /// [`AppError::Backend`] when the backend cannot queue the command and
    /// [`AppError::Store`] when the change cannot be saved.
    pub fn cancel(&mut self) -> Result<(), AppError> {
        let id = self.current.clone().ok_or(AppError::NoCurrentSession)?;
        if self.queued.remove(&id).is_some() {
            return self.change_session(id, |session| session.status = Status::Idle);
        }
        self.backend.cancel(id)?;
        Ok(())
    }

    /// Answers a permission request with `option`, or cancels it with `None`.
    ///
    /// The answer goes into the transcript, and the session runs again once none of its
    /// requests is waiting. A request that is not waiting (already answered, settled by the
    /// end of its turn, or unknown) is ignored.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Backend`] when the backend cannot queue the answer (the request
    /// keeps waiting) and [`AppError::Store`] when the change cannot be saved.
    pub fn answer_permission(
        &mut self,
        session: SessionId,
        permission: PermissionId,
        option: Option<PermissionOptionId>,
    ) -> Result<(), AppError> {
        let waiting = (session, permission);
        if !self.permissions.contains(&waiting) {
            return Ok(());
        }
        let (session, permission) = waiting;
        self.backend
            .respond_permission(session.clone(), permission, option.clone())?;
        self.permissions.remove(&(session.clone(), permission));
        let answered = Update::PermissionAnswered {
            id: permission,
            option,
        };
        self.apply_update(&session, &answered)?;
        if self
            .permissions
            .iter()
            .any(|(waiting, _)| *waiting == session)
        {
            return Ok(());
        }
        self.change_session(session, |answered| answered.status = Status::Running)
    }

    /// Grants the session's future permission requests without asking while `enabled`.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists and
    /// [`AppError::Store`] when the change cannot be saved.
    pub fn set_auto_allow(&mut self, session: SessionId, enabled: bool) -> Result<(), AppError> {
        self.change_session(session, |changed| changed.auto_allow = enabled)
    }

    /// Switches the session to the model `value`.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::NoSuchConfig`] when its backend offers no model option,
    /// [`AppError::Backend`] when the backend cannot queue the command and
    /// [`AppError::Store`] when the change cannot be saved.
    pub fn set_model(&mut self, session: SessionId, value: ConfigValueId) -> Result<(), AppError> {
        self.set_config(&session, ConfigCategory::Model, &value)?;
        self.change_session(session, |changed| changed.model = Some(value))
    }

    /// Switches the session to the reasoning effort `value`.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::NoSuchConfig`] when its backend offers no effort option,
    /// [`AppError::Backend`] when the backend cannot queue the command and
    /// [`AppError::Store`] when the change cannot be saved.
    pub fn set_effort(&mut self, session: SessionId, value: ConfigValueId) -> Result<(), AppError> {
        self.set_config(&session, ConfigCategory::Effort, &value)?;
        self.change_session(session, |changed| changed.effort = Some(value))
    }

    /// The view of the selected session, or `None` when no session is selected.
    pub fn session_view(&self) -> Option<SessionViewModel<'_>> {
        let session = self.session(self.current.as_ref()?).ok()?;
        let project = self.project(&session.project).ok()?;
        let transcript = self.transcripts.get(&session.id)?;
        Some(SessionViewModel::build(
            project,
            &self.sessions,
            session,
            transcript,
            self.config_of(&session.id),
        ))
    }

    /// The environment of the selected session, or `None` when no session is selected.
    ///
    /// The instruction files are read from disk on every call, so the view shows them as
    /// they are now.
    pub fn env_view(&self) -> Option<EnvViewModel> {
        let session = self.session(self.current.as_ref()?).ok()?;
        let project = self.project(&session.project).ok()?;
        Some(EnvViewModel::build(
            project,
            &self.sessions,
            session,
            session_instructions(project, session),
            self.config_of(&session.id),
        ))
    }

    /// Applies something the backend reported.
    ///
    /// [`BackendEvent::SessionCreated`] and [`BackendEvent::SessionLoaded`] store the
    /// session's config options (the former also its backend id), settle the request and
    /// send the prompts that were waiting for the session.
    ///
    /// [`BackendEvent::RequestFailed`] for a load starts a fresh backend session instead:
    /// the agent forgets the earlier turns, so the instructions are sent again and a notice
    /// says so; the waiting prompts go to the fresh session. Any other failed request marks
    /// the session [`Failed`](Status::Failed), drops its waiting prompts and tells the user
    /// why in a notice.
    ///
    /// [`BackendEvent::Update`] goes into the store and then the session's transcript. A
    /// permission request makes the session [`WaitingForYou`](Status::WaitingForYou), or
    /// with auto-allow is answered at once with the first "allow once" option (else the first
    /// "allow always" one); the end of a turn makes it [`Done`](Status::Done), a failure
    /// [`Failed`](Status::Failed); both settle its waiting permission requests. New config
    /// options replace the session's and set its model and effort to their current values.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when the event names a session that does not
    /// exist, [`AppError::Backend`] when a follow-up command cannot be queued and
    /// [`AppError::Store`] when the change cannot be saved.
    pub fn apply_event(&mut self, event: BackendEvent) -> Result<(), AppError> {
        match event {
            BackendEvent::SessionCreated {
                request,
                session,
                backend_session,
                config,
            } => {
                self.session_mut(&session)?.backend_session = Some(backend_session);
                self.attach(request, &session, config)
            }
            BackendEvent::SessionLoaded {
                request,
                session,
                config,
            } => {
                self.session(&session)?;
                self.attach(request, &session, config)
            }
            BackendEvent::Update { session, update } => {
                self.apply_update(&session, &update)?;
                self.react(&session, update)
            }
            BackendEvent::RequestFailed {
                request,
                session,
                error,
            } => {
                self.session(&session)?;
                match self.pending.remove(&request) {
                    Some((_, Request::Load)) => self.fall_back_to_new_session(&session),
                    _ => self.fail_session(&session, error),
                }
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

    /// Applies `change` to the session and saves its project's session list.
    fn change_session(
        &mut self,
        id: SessionId,
        change: impl FnOnce(&mut Session),
    ) -> Result<(), AppError> {
        let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) else {
            return Err(AppError::UnknownSession(id));
        };
        change(session);
        let project = session.project.clone();
        self.save_sessions(&project)
    }

    fn config_of(&self, session: &SessionId) -> &[ConfigOption] {
        self.config.get(session).map_or(&[], Vec::as_slice)
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

    /// Asks the backend to load the session's stored backend session, or to create one when
    /// it has none, unless a request for the session is already pending.
    fn request_backend_session(&mut self, id: &SessionId) -> Result<(), AppError> {
        if self.pending.values().any(|(session, _)| session == id) {
            return Ok(());
        }
        let request = self.next_request();
        let session = self.session(id)?;
        let kind = if let Some(backend_session) = &session.backend_session {
            self.backend
                .load_session(request, id.clone(), backend_session, &session.cwd)?;
            Request::Load
        } else {
            self.backend
                .new_session(request, id.clone(), &session.cwd)?;
            Request::Create
        };
        self.pending.insert(request, (id.clone(), kind));
        Ok(())
    }

    /// Settles a successful create or load: the session is attached, its config options are
    /// stored and its waiting prompts are sent.
    fn attach(
        &mut self,
        request: RequestId,
        session: &SessionId,
        config: Vec<ConfigOption>,
    ) -> Result<(), AppError> {
        self.pending.remove(&request);
        self.config.insert(session.clone(), config);
        self.attached.insert(session.clone());
        for prompt in self.queued.remove(session).unwrap_or_default() {
            let skipped = self.send_prompt(session, prompt)?;
            self.report_skipped(session, skipped)?;
        }
        self.change_session(session.clone(), |_| {})
    }

    /// Replaces a stored backend session that no longer loads with a fresh one.
    fn fall_back_to_new_session(&mut self, id: &SessionId) -> Result<(), AppError> {
        self.change_session(id.clone(), |session| {
            session.backend_session = None;
            session.instructions_injected = false;
        })?;
        self.request_backend_session(id)?;
        self.apply_update(id, &Update::Notice(RESUMED_NOTICE.into()))
    }

    fn fail_session(&mut self, id: &SessionId, error: String) -> Result<(), AppError> {
        self.queued.remove(id);
        self.change_session(id.clone(), |session| session.status = Status::Failed)?;
        self.apply_update(id, &Update::Notice(error))
    }

    /// Sends `prompt` to the session's attached backend session, with the instruction chain
    /// before the text when the session has not received it yet. Returns the instruction
    /// files that had to be skipped; the caller saves the session.
    fn send_prompt(
        &mut self,
        id: &SessionId,
        prompt: QueuedPrompt,
    ) -> Result<Vec<SkippedInstruction>, AppError> {
        let session = self.session(id)?;
        let (text, skipped) = if session.instructions_injected {
            (prompt.text, Vec::new())
        } else {
            let chain = session_instructions(self.project(&session.project)?, session);
            (inject(&chain, &prompt.text), chain.skipped)
        };
        let mut content = Vec::new();
        if !text.is_empty() {
            content.push(PromptContent::Text(text));
        }
        content.extend(prompt.attachments);
        self.backend.prompt(id.clone(), content)?;
        self.session_mut(id)?.instructions_injected = true;
        Ok(skipped)
    }

    fn report_skipped(
        &mut self,
        session: &SessionId,
        skipped: Vec<SkippedInstruction>,
    ) -> Result<(), AppError> {
        for file in skipped {
            let notice = format!(
                "instruction file skipped: {}: {}",
                file.path.display(),
                file.reason
            );
            self.apply_update(session, &Update::Notice(notice))?;
        }
        Ok(())
    }

    /// Updates the session's status and config after `update` was applied.
    fn react(&mut self, session: &SessionId, update: Update) -> Result<(), AppError> {
        match update {
            Update::PermissionRequested { id, options, .. } => {
                self.permission_requested(session, id, &options)
            }
            Update::TurnEnded(_) => self.end_turn(session, Status::Done),
            Update::Failed(_) => self.end_turn(session, Status::Failed),
            Update::ConfigOptions(options) => self.replace_config(session, options),
            _ => Ok(()),
        }
    }

    fn permission_requested(
        &mut self,
        session: &SessionId,
        permission: PermissionId,
        options: &[PermissionOption],
    ) -> Result<(), AppError> {
        let automatic = if self.session(session)?.auto_allow {
            allowing_option(options)
        } else {
            None
        };
        self.permissions.insert((session.clone(), permission));
        match automatic {
            Some(option) => self.answer_permission(session.clone(), permission, Some(option)),
            None => self.change_session(session.clone(), |waiting| {
                waiting.status = Status::WaitingForYou;
            }),
        }
    }

    /// Ends the session's turn with `status`; its unanswered permission requests lapse.
    fn end_turn(&mut self, session: &SessionId, status: Status) -> Result<(), AppError> {
        self.permissions.retain(|(waiting, _)| waiting != session);
        self.change_session(session.clone(), |ended| ended.status = status)
    }

    fn replace_config(
        &mut self,
        session: &SessionId,
        options: Vec<ConfigOption>,
    ) -> Result<(), AppError> {
        let current = |category: ConfigCategory| {
            options
                .iter()
                .find(|option| option.category == category)
                .map(|option| option.current.clone())
        };
        let model = current(ConfigCategory::Model);
        let effort = current(ConfigCategory::Effort);
        self.config.insert(session.clone(), options);
        self.change_session(session.clone(), |changed| {
            changed.model = model;
            changed.effort = effort;
        })
    }

    /// Asks the backend to set the session's option of `category` to `value` and records the
    /// value as the option's current one.
    fn set_config(
        &mut self,
        session: &SessionId,
        category: ConfigCategory,
        value: &ConfigValueId,
    ) -> Result<(), AppError> {
        self.session(session)?;
        let Some(option) = self.config.get_mut(session).and_then(|options| {
            options
                .iter_mut()
                .find(|option| option.category == category)
        }) else {
            return Err(AppError::NoSuchConfig(category));
        };
        self.backend
            .set_config(session.clone(), &option.id, value)?;
        option.current = value.clone();
        Ok(())
    }

    /// Writes `update` to the store, then applies it to the session's transcript when that is
    /// loaded; an unloaded transcript picks it up from the store when it is selected. An
    /// update the store rejects never reaches the transcript.
    fn apply_update(&mut self, session: &SessionId, update: &Update) -> Result<(), AppError> {
        self.session(session)?;
        self.store.append_update(session.clone(), update)?;
        if let Some(transcript) = self.transcripts.get_mut(session) {
            transcript.apply(update);
        }
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
}

/// The instruction files that apply to `session`: the project's, plus its repository's for a
/// repository session.
fn session_instructions(project: &Project, session: &Session) -> InstructionChain {
    let repository = match &session.scope {
        Scope::Project => None,
        Scope::Repository(id) => project
            .repositories
            .iter()
            .find(|repository| &repository.id == id),
    };
    instruction_chain(project, repository)
}

/// The option an auto-allowing session answers with: the first that allows once, else the
/// first that allows always.
fn allowing_option(options: &[PermissionOption]) -> Option<PermissionOptionId> {
    [PermissionKind::AllowOnce, PermissionKind::AllowAlways]
        .into_iter()
        .find_map(|kind| options.iter().find(|option| option.kind == kind))
        .map(|option| option.id.clone())
}

/// Converts an attachment into prompt content: an image is read and inlined, anything else
/// becomes a link.
fn prompt_content(attachment: Attachment) -> Result<PromptContent, AppError> {
    let Some(mime) = image_mime(&attachment.path) else {
        return Ok(PromptContent::FileLink {
            path: attachment.path,
        });
    };
    match fs::read(&attachment.path) {
        Ok(bytes) => Ok(PromptContent::Image {
            mime: mime.into(),
            base64: BASE64.encode(bytes),
        }),
        Err(source) => Err(AppError::Attachment {
            path: attachment.path,
            source,
        }),
    }
}

/// The media type of an image the agent can be sent inline, judged by the file extension.
fn image_mime(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests;
