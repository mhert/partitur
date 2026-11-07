//! [`App`], the core's single entry point for the UI.
//!
//! The app is a synchronous struct behind two ports: a [`Backend`] that runs the agents and a
//! [`Store`] that persists what the user has opened. The UI calls its methods, feeds it the
//! backend's [`BackendEvent`]s and renders the view-models it returns.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use thiserror::Error;

use crate::backend::{
    Backend, BackendError, BackendEvent, ChildOutcome, ConfigCategory, ConfigOption,
    PermissionKind, PermissionOption, PromptContent, ToolKind, TurnOrigin, Update,
};
use crate::ids::{
    BackendId, BackendInfo, ConfigValueId, PermissionId, PermissionOptionId, ProjectId,
    RepositoryId, RequestId, SessionId,
};
use crate::instructions::{InstructionChain, SkippedInstruction, inject, instruction_chain};
use crate::orchestrator::{
    PARTITUR_TOOL_PREFIX, denial_notice, is_orchestrator, preamble, restrained,
};
use crate::project::{Project, ProjectError, discover_project, rescan};
use crate::session::{Scope, Session, Status, title_from_prompt};
use crate::store::{Store, StoreError};
use crate::transcript::Transcript;
use crate::view_model::{EnvViewModel, SessionViewModel, TreeModel};

mod orchestration;

use orchestration::Generated;

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
    /// The current session is still running a turn or waiting for a permission answer, so
    /// it takes no prompt until the turn ends or is stopped.
    #[error("a turn is still running; stop it or wait for it to end")]
    TurnInProgress,
    /// No backend is configured, or the default names none of the configured ones.
    #[error("no configured backend matches the default backend")]
    NoBackends,
    /// No configured backend has this id: the session's backend was removed from the
    /// configuration, or a switch named a backend that is not configured.
    #[error("backend {0} is no longer configured; pick another backend for this session")]
    UnknownBackend(BackendId),
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
    /// The configured backends, in the order `load` received them (sorted by id, as the
    /// binary passes them).
    backends: Vec<BackendInfo>,
    /// The backend new sessions run on; always one of `backends`.
    default: BackendId,
    /// The projects, open and closed, in the order they were first opened.
    projects: Vec<Project>,
    /// The sessions of every project, in the order they were created. A closed project's
    /// sessions stay here, so events that arrive after closing it still apply.
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
    /// Permission requests waiting for the user's answer, with the options they offer.
    permissions: HashMap<(SessionId, PermissionId), Vec<PermissionOption>>,
    /// Who started each session's running turn; taken when the turn ends.
    turn_origins: HashMap<SessionId, TurnOrigin>,
    /// Prompts Partitur generated for a session while it was running or waiting, oldest
    /// first; one goes out each time its turn ends. Kept in memory only: a restart drops them.
    deferred: HashMap<SessionId, VecDeque<Generated>>,
    next_request: u64,
    current: Option<SessionId>,
}

impl App {
    /// Restores the projects and sessions the store holds, closed projects included; only
    /// open ones appear in the [`tree`](Self::tree).
    ///
    /// `backends` are the configured backends and `default` the one new sessions run on. A
    /// stored session saved before sessions named their backend is given `default`, and its
    /// project's session list is saved once.
    ///
    /// A session cannot be running or waiting after a restart, so every session starts
    /// [`Idle`](Status::Idle). The backend is not asked to resume anything yet.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoBackends`] when `backends` is empty or does not contain
    /// `default`, and [`AppError::Store`] when the projects or their sessions cannot be
    /// loaded or a migrated session list cannot be saved.
    pub fn load(
        backend: Box<dyn Backend>,
        store: Box<dyn Store>,
        backends: Vec<BackendInfo>,
        default: BackendId,
    ) -> Result<Self, AppError> {
        if !backends.iter().any(|info| info.id == default) {
            return Err(AppError::NoBackends);
        }
        let projects = store.load_projects()?;
        let mut sessions = Vec::new();
        for project in &projects {
            let mut loaded = store.load_sessions(project.id.clone())?;
            let mut migrated = false;
            for session in loaded
                .iter_mut()
                .filter(|session| session.backend.is_unset())
            {
                session.backend = default.clone();
                migrated = true;
            }
            if migrated {
                store.save_sessions(project.id.clone(), &loaded)?;
            }
            sessions.extend(loaded);
        }
        for session in &mut sessions {
            session.status = Status::Idle;
        }
        Ok(Self {
            backend,
            store,
            backends,
            default,
            projects,
            sessions,
            transcripts: HashMap::new(),
            config: HashMap::new(),
            pending: HashMap::new(),
            attached: HashSet::new(),
            queued: HashMap::new(),
            permissions: HashMap::new(),
            turn_origins: HashMap::new(),
            deferred: HashMap::new(),
            next_request: 0,
            current: None,
        })
    }

    /// The configured backends, in the order [`load`](Self::load) received them (sorted by
    /// id, as the binary passes them).
    pub fn backends(&self) -> &[BackendInfo] {
        &self.backends
    }

    /// The backend new sessions run on.
    pub fn default_backend(&self) -> &BackendId {
        &self.default
    }

    /// Opens the folder `root` as a project and saves the project list.
    ///
    /// The project is stored under the canonical form of `root`. A folder that is already
    /// known (compared by canonical path) is rescanned instead and keeps its id; a closed
    /// one is reopened with its sessions.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Project`] when `root` is not a readable directory (a path that
    /// cannot be resolved is [`ProjectError::NotADirectory`]) and [`AppError::Store`] when
    /// the project list cannot be saved.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use partitur_core::App;
    /// use partitur_core::testing::{MemoryStore, RecordingBackend, single_claude};
    ///
    /// let dir = tempfile::tempdir()?;
    /// std::fs::create_dir_all(dir.path().join("api/.git"))?;
    /// let (backends, default) = single_claude();
    /// let backend = Box::new(RecordingBackend::new());
    /// let mut app = App::load(backend, Box::new(MemoryStore::new()), backends, default)?;
    ///
    /// let project = app.open_project(dir.path())?;
    ///
    /// assert_eq!(app.open_project(dir.path())?, project);
    /// assert_eq!(app.tree().projects[0].repositories[0].name, "api");
    /// # Ok(())
    /// # }
    /// ```
    pub fn open_project(&mut self, root: &Path) -> Result<ProjectId, AppError> {
        let root = fs::canonicalize(root).map_err(|_| ProjectError::NotADirectory {
            path: root.to_path_buf(),
        })?;
        let existing = self
            .projects
            .iter_mut()
            .find(|project| fs::canonicalize(&project.root).is_ok_and(|known| known == root));
        if let Some(project) = existing {
            rescan(project)?;
            project.closed = false;
            let id = project.id.clone();
            self.save_projects()?;
            return Ok(id);
        }
        let project = discover_project(&root)?;
        let id = project.id.clone();
        self.projects.push(project);
        self.save_projects()?;
        Ok(id)
    }

    /// Closes a project: stops its sessions' turns, takes it out of the tree, deselects its
    /// session and saves the project list.
    ///
    /// Each session that is running or waiting is stopped like [`cancel`](Self::cancel)
    /// stops the current one; a session the backend no longer knows has no turn to stop.
    /// The project and its sessions stay saved, and opening the folder again restores them;
    /// there is no way to delete them.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownProject`] when no open project has this id,
    /// [`AppError::Backend`] when a turn cannot be stopped (the project stays open then) and
    /// [`AppError::Store`] when the change cannot be saved.
    pub fn close_project(&mut self, project: ProjectId) -> Result<(), AppError> {
        if !self
            .projects
            .iter()
            .any(|open| open.id == project && !open.closed)
        {
            return Err(AppError::UnknownProject(project));
        }
        let turns: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|session| {
                session.project == project
                    && matches!(session.status, Status::Running | Status::WaitingForYou)
            })
            .map(|session| session.id.clone())
            .collect();
        for session in turns {
            match self.stop_turn(session) {
                Ok(()) | Err(AppError::Backend(BackendError::UnknownSession(_))) => {}
                Err(error) => return Err(error),
            }
        }
        if let Some(current) = &self.current
            && self.session(current)?.project == project
        {
            self.current = None;
        }
        for closed in &mut self.projects {
            if closed.id == project {
                closed.closed = true;
            }
        }
        self.save_projects()
    }

    /// Creates a session in `project`, asks the backend to start it and selects it.
    ///
    /// The session runs on the [default backend](Self::default_backend), in the project
    /// root or in the repository's folder, starts
    /// [`Idle`](Status::Idle) and is titled "New session". It has no backend id until the
    /// backend answers with [`BackendEvent::SessionCreated`]. A project session in a project
    /// with at least two repositories besides its root is an orchestrator.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownProject`] or [`AppError::UnknownRepository`] when `project`
    /// or the scope's repository does not exist, [`AppError::Backend`] when the backend cannot
    /// queue the command (no session is created then) and [`AppError::Store`] when the
    /// session list cannot be saved.
    pub fn new_session(&mut self, project: ProjectId, scope: Scope) -> Result<SessionId, AppError> {
        let cwd = self.working_directory(&project, &scope)?;
        let orchestrator = is_orchestrator(self.project(&project)?, &scope);
        let session = Session {
            id: SessionId::new(),
            project,
            scope,
            cwd,
            title: NEW_SESSION_TITLE.into(),
            backend_session: None,
            backend: self.default.clone(),
            model: None,
            effort: None,
            status: Status::Idle,
            parent: None,
            auto_allow: false,
            instructions_injected: false,
            created_at: unix_seconds(),
            orchestrator,
        };
        let request = self.next_request();
        self.backend.new_session(
            request,
            session.id.clone(),
            &session.backend,
            &session.cwd,
            session.orchestrator,
        )?;
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

    /// Builds the navigation tree: open projects in the order they were opened, each with
    /// its sessions newest first (children nested under their parent) and its repositories
    /// as discovered. Sessions created within the same second count the later one as newer.
    pub fn tree(&self) -> TreeModel {
        TreeModel::build(&self.projects, &self.sessions)
    }

    /// Sends a prompt to the current session.
    ///
    /// Text that is blank with no attachments does nothing. The first prompt names an untitled
    /// session. The prompt that goes to the backend carries the session's instruction chain
    /// (see [`inject`]) as long as the session has not received it, and an orchestrator's
    /// carries the [`preamble`] between the instructions and the text; each instruction file
    /// that cannot be read is reported in a notice. The transcript records the text as typed.
    ///
    /// A session whose backend session is not attached yet (still being created, never
    /// created, or restored from the store) queues the prompt and asks the backend to create
    /// or load it; the prompt goes out once that succeeds. Either way the session becomes
    /// [`Running`](Status::Running).
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoCurrentSession`] when no session is selected,
    /// [`AppError::UnknownBackend`] when the session's backend is no longer configured,
    /// [`AppError::TurnInProgress`] when the session is [`Running`](Status::Running) or
    /// [`WaitingForYou`](Status::WaitingForYou),
    /// [`AppError::Attachment`] when an attached image cannot be read,
    /// [`AppError::Backend`] when the backend cannot queue the command (nothing changes in
    /// these cases) and [`AppError::Store`] when the change cannot be saved.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use partitur_core::testing::{Call, MemoryStore, RecordingBackend, single_claude};
    /// use partitur_core::{App, BackendEvent, BackendSessionId, PromptContent, Scope};
    ///
    /// let dir = tempfile::tempdir()?;
    /// let backend = RecordingBackend::new();
    /// let (backends, default) = single_claude();
    /// let store = Box::new(MemoryStore::new());
    /// let mut app = App::load(Box::new(backend.clone()), store, backends, default)?;
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
        let session = self.session(&id)?;
        if !self.is_configured(&session.backend) {
            return Err(AppError::UnknownBackend(session.backend.clone()));
        }
        if matches!(session.status, Status::Running | Status::WaitingForYou) {
            return Err(AppError::TurnInProgress);
        }
        if text.trim().is_empty() && attachments.is_empty() {
            return Ok(());
        }
        let attachments = attachments
            .into_iter()
            .map(prompt_content)
            .collect::<Result<_, _>>()?;
        self.start_turn(
            &id,
            &Update::UserText(text.to_owned()),
            text.to_owned(),
            attachments,
            TurnOrigin::User,
        )?;
        let title = title_from_prompt(text);
        if self.session(&id)?.title != NEW_SESSION_TITLE || title.is_empty() {
            return Ok(());
        }
        self.change_session(id, |session| session.title = title)
    }

    /// Starts a turn of the session with `text` and `attachments`, started by `origin`, and
    /// records `record` in its transcript.
    ///
    /// The prompt goes to the attached backend session (with the instructions and an
    /// orchestrator's preamble as [`submit_prompt`](Self::submit_prompt) describes), or waits
    /// for the backend session to be created or loaded. Each instruction file that cannot be
    /// read is reported in a notice, and the session becomes [`Running`](Status::Running).
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::Backend`] when the backend cannot queue the command (nothing changes then)
    /// and [`AppError::Store`] when the change cannot be saved.
    fn start_turn(
        &mut self,
        id: &SessionId,
        record: &Update,
        text: String,
        attachments: Vec<PromptContent>,
        origin: TurnOrigin,
    ) -> Result<(), AppError> {
        let prompt = QueuedPrompt { text, attachments };
        let skipped = if self.is_attached(id)? {
            self.send_prompt(id, prompt)?
        } else {
            self.request_backend_session(id.clone())?;
            self.queued.entry(id.clone()).or_default().push(prompt);
            Vec::new()
        };
        self.turn_origins.insert(id.clone(), origin);
        self.apply_update(id, record)?;
        self.report_skipped(id, skipped)?;
        self.change_session(id.clone(), |session| session.status = Status::Running)
    }

    /// Stops the current session's turn.
    ///
    /// A running turn's waiting permission requests are cancelled first (an agent blocked on
    /// one would never end the turn), then the backend cancels the turn and the session stays
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
        self.stop_turn(id)
    }

    /// Stops the session's turn as [`cancel`](Self::cancel) describes.
    fn stop_turn(&mut self, id: SessionId) -> Result<(), AppError> {
        if self.queued.remove(&id).is_some() {
            return self.change_session(id, |session| session.status = Status::Idle);
        }
        for (permission, _) in self.waiting_permissions(&id) {
            self.answer_permission(id.clone(), permission, None)?;
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
        if !self.permissions.contains_key(&waiting) {
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
            .keys()
            .any(|(waiting, _)| *waiting == session)
        {
            return Ok(());
        }
        self.change_session(session, |answered| answered.status = Status::Running)
    }

    /// Grants the session's permission requests without asking while `enabled`.
    ///
    /// Enabling it also answers the requests already waiting, by the same rule as new ones:
    /// the first "allow once" option, else the first "allow always" one; a request offering
    /// neither keeps waiting.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::Backend`] when an answer cannot be queued and [`AppError::Store`] when
    /// the change cannot be saved.
    pub fn set_auto_allow(&mut self, session: SessionId, enabled: bool) -> Result<(), AppError> {
        if enabled {
            for (permission, options) in self.waiting_permissions(&session) {
                if let Some(option) = allowing_option(&options) {
                    self.answer_permission(session.clone(), permission, Some(option))?;
                }
            }
        }
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

    /// Moves the session to the configured backend `backend`.
    ///
    /// The session is detached from its current agent: a create or load still on its way is
    /// forgotten (its answer is ignored when it arrives), waiting permission requests lapse
    /// as cancelled without telling the backend, the backend session id is dropped and the
    /// instructions count as not sent. The old backend's config options and the model and
    /// effort chosen from them are forgotten, since their ids mean nothing to another backend.
    /// The session becomes [`Idle`](Status::Idle) and, when its transcript has entries, a
    /// notice says the new agent remembers none of them. Once that is saved, a backend session
    /// is requested from `backend` right away, so the new agent's options (and with them the
    /// model and effort pickers) arrive before the first prompt; the session stays
    /// [`Idle`](Status::Idle) while the create is pending. The first prompt to the new agent
    /// carries the instructions again. Switching to the backend the session already runs on
    /// changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownBackend`] when `backend` is not configured,
    /// [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::TurnInProgress`] when the session is [`Running`](Status::Running) or
    /// [`WaitingForYou`](Status::WaitingForYou), [`AppError::Store`] when the session's
    /// transcript cannot be read (nothing changes in these cases) or the change cannot be
    /// saved, and [`AppError::Backend`] when the backend cannot queue the create. The switch
    /// has been made and saved by then; the next prompt asks for the backend session again.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use partitur_core::testing::{Call, MemoryStore, RecordingBackend};
    /// use partitur_core::{App, BackendId, BackendInfo, Scope};
    ///
    /// let dir = tempfile::tempdir()?;
    /// let (claude, work) = (BackendId::try_from("claude")?, BackendId::try_from("work")?);
    /// let backends = [&claude, &work]
    ///     .map(|id| BackendInfo {
    ///         id: id.clone(),
    ///         name: id.to_string(),
    ///         command: "claude-agent-acp".into(),
    ///     })
    ///     .to_vec();
    /// let backend = RecordingBackend::new();
    /// let store = Box::new(MemoryStore::new());
    /// let mut app = App::load(Box::new(backend.clone()), store, backends, claude)?;
    /// let project = app.open_project(dir.path())?;
    /// let session = app.new_session(project, Scope::Project)?;
    ///
    /// app.switch_backend(session, work.clone())?;
    ///
    /// // The new agent is started at once; nothing was said yet, so there is no notice.
    /// assert!(matches!(
    ///     backend.calls().last(),
    ///     Some(Call::NewSession { backend, .. }) if *backend == work
    /// ));
    /// let view = app.session_view().ok_or("no session")?;
    /// assert_eq!(view.backend, work);
    /// assert!(view.entries.is_empty());
    /// assert!(!view.running);
    /// # Ok(())
    /// # }
    /// ```
    pub fn switch_backend(
        &mut self,
        session: SessionId,
        backend: BackendId,
    ) -> Result<(), AppError> {
        if !self.is_configured(&backend) {
            return Err(AppError::UnknownBackend(backend));
        }
        let current = self.session(&session)?;
        if matches!(current.status, Status::Running | Status::WaitingForYou) {
            return Err(AppError::TurnInProgress);
        }
        if current.backend == backend {
            return Ok(());
        }
        let notify = self.has_entries(&session)?;
        self.attached.remove(&session);
        self.queued.remove(&session);
        self.config.remove(&session);
        self.pending.retain(|_, (pending, _)| *pending != session);
        self.lapse_permissions(&session)?;
        if notify {
            let notice =
                format!("switched to {backend}; the new agent has no memory of earlier turns");
            self.apply_update(&session, &Update::Notice(notice))?;
        }
        self.change_session(session.clone(), |switched| {
            switched.backend = backend;
            switched.backend_session = None;
            switched.instructions_injected = false;
            switched.model = None;
            switched.effort = None;
            switched.status = Status::Idle;
        })?;
        self.request_backend_session(session)
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
            &self.backends,
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
            &self.backends,
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
    /// says so; the waiting prompts go to the fresh session. A failed create marks the
    /// session [`Failed`](Status::Failed), drops its waiting prompts and tells the user why in
    /// a notice.
    ///
    /// Any of these three for a request that is no longer pending (the session was switched
    /// to another backend meanwhile) is ignored.
    ///
    /// [`BackendEvent::SessionClosed`] detaches the session: its waiting permission requests
    /// lapse as cancelled, a notice says why, a running or waiting session becomes
    /// [`Failed`](Status::Failed), and the next prompt loads the stored backend session again.
    ///
    /// [`BackendEvent::Update`] goes into the store and then the session's transcript. In an
    /// orchestrator session a permission request for one of Partitur's own tools is allowed at
    /// once when it offers an allowing option, and one that would edit, delete, move or
    /// execute is rejected at once (cancelled when it offers no rejection) with a notice
    /// saying so. Any other permission request
    /// makes the session [`WaitingForYou`](Status::WaitingForYou), or
    /// with auto-allow is answered at once with the first "allow once" option (else the first
    /// "allow always" one); the end of a turn makes it [`Done`](Status::Done), a failure
    /// [`Failed`](Status::Failed); both settle its waiting permission requests. New config
    /// options replace the session's and set its model and effort to their current values.
    ///
    /// An update or a close for a session that is not attached comes from a connection the
    /// session no longer uses (it switched backends, or its connection already closed) and
    /// is ignored.
    ///
    /// [`BackendEvent::ToolCalled`] is ignored.
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
                if self.settle(request, &session)?.is_none() {
                    return Ok(());
                }
                self.change_session(session.clone(), |created| {
                    created.backend_session = Some(backend_session);
                })?;
                self.attach(&session, config)
            }
            BackendEvent::SessionLoaded {
                request,
                session,
                config,
            } => match self.settle(request, &session)? {
                Some(_) => self.attach(&session, config),
                None => Ok(()),
            },
            BackendEvent::Update { session, update } => {
                if !self.is_attached(&session)? {
                    return Ok(());
                }
                self.apply_update(&session, &update)?;
                self.react(&session, update)
            }
            BackendEvent::SessionClosed { session, error } => {
                if !self.is_attached(&session)? {
                    return Ok(());
                }
                self.close_session(&session, &error)
            }
            BackendEvent::RequestFailed {
                request,
                session,
                error,
            } => match self.settle(request, &session)? {
                Some(Request::Load) => self.fall_back_to_new_session(&session),
                Some(Request::Create) => self.fail_session(&session, error),
                None => Ok(()),
            },
            BackendEvent::ToolCalled { .. } => Ok(()),
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

    /// Whether the session is attached: its backend session was created or loaded by this
    /// process, and it has neither closed nor switched backends since.
    fn is_attached(&self, id: &SessionId) -> Result<bool, AppError> {
        self.session(id)?;
        Ok(self.attached.contains(id))
    }

    /// Whether the session's transcript has at least one entry. A transcript that is not
    /// loaded yet is read from the store without keeping it.
    fn has_entries(&self, id: &SessionId) -> Result<bool, AppError> {
        let empty = match self.transcripts.get(id) {
            Some(transcript) => transcript.entries().is_empty(),
            None => Transcript::from_updates(&self.store.load_updates(id.clone())?)
                .entries()
                .is_empty(),
        };
        Ok(!empty)
    }

    /// Whether `backend` is one of the configured backends.
    fn is_configured(&self, backend: &BackendId) -> bool {
        self.backends.iter().any(|info| &info.id == backend)
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

    /// Asks the session's backend to load its stored backend session, or to create one when
    /// it has none, unless a request for the session is already pending. The request is
    /// recorded as pending under `id`.
    fn request_backend_session(&mut self, id: SessionId) -> Result<(), AppError> {
        if self.pending.values().any(|(session, _)| *session == id) {
            return Ok(());
        }
        let request = self.next_request();
        let session = self.session(&id)?;
        let kind = if let Some(backend_session) = &session.backend_session {
            self.backend.load_session(
                request,
                id.clone(),
                &session.backend,
                backend_session,
                &session.cwd,
                session.orchestrator,
            )?;
            Request::Load
        } else {
            self.backend.new_session(
                request,
                id.clone(),
                &session.backend,
                &session.cwd,
                session.orchestrator,
            )?;
            Request::Create
        };
        self.pending.insert(request, (id, kind));
        Ok(())
    }

    /// Takes the answered `request` off the pending ones and returns what it asked for, or
    /// `None` when it is no longer pending.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when `session` does not exist.
    fn settle(
        &mut self,
        request: RequestId,
        session: &SessionId,
    ) -> Result<Option<Request>, AppError> {
        self.session(session)?;
        Ok(self.pending.remove(&request).map(|(_, kind)| kind))
    }

    /// Completes a successful create or load: the session is attached, its config options
    /// are stored, the model and effort it recorded are re-applied where the backend reports
    /// a different current value, and its waiting prompts are sent.
    fn attach(&mut self, session: &SessionId, config: Vec<ConfigOption>) -> Result<(), AppError> {
        self.config.insert(session.clone(), config);
        self.attached.insert(session.clone());
        self.reapply_choices(session)?;
        for prompt in self.queued.remove(session).unwrap_or_default() {
            let skipped = self.send_prompt(session, prompt)?;
            self.report_skipped(session, skipped)?;
        }
        self.change_session(session.clone(), |_| {})
    }

    /// Sends the session's recorded model and effort to the backend where its config options
    /// report another current value; a category the backend does not offer is left alone.
    fn reapply_choices(&mut self, session: &SessionId) -> Result<(), AppError> {
        let recorded = self.session(session)?;
        let choices = [
            (ConfigCategory::Model, recorded.model.clone()),
            (ConfigCategory::Effort, recorded.effort.clone()),
        ];
        for (category, value) in choices {
            let Some(value) = value else { continue };
            let differs = self
                .config_of(session)
                .iter()
                .any(|option| option.category == category && option.current != value);
            if differs {
                self.set_config(session, category, &value)?;
            }
        }
        Ok(())
    }

    /// The session's permission requests waiting for an answer, oldest first, with their
    /// options.
    fn waiting_permissions(
        &self,
        session: &SessionId,
    ) -> Vec<(PermissionId, Vec<PermissionOption>)> {
        let mut waiting: Vec<_> = self
            .permissions
            .iter()
            .filter(|((waiting, _), _)| waiting == session)
            .map(|((_, permission), options)| (*permission, options.clone()))
            .collect();
        waiting.sort_by_key(|(permission, _)| permission.0);
        waiting
    }

    /// Replaces a stored backend session that no longer loads with a fresh one.
    fn fall_back_to_new_session(&mut self, id: &SessionId) -> Result<(), AppError> {
        self.change_session(id.clone(), |session| {
            session.backend_session = None;
            session.instructions_injected = false;
        })?;
        self.request_backend_session(id.clone())?;
        self.apply_update(id, &Update::Notice(RESUMED_NOTICE.into()))
    }

    /// Forgets a backend session that ended outside a turn. Its waiting permission requests
    /// lapse as cancelled (the agent can no longer receive answers), a notice says why, and a
    /// running or waiting session fails. The stored backend id is kept, so the next prompt
    /// loads it again.
    fn close_session(&mut self, id: &SessionId, error: &str) -> Result<(), AppError> {
        self.session(id)?;
        self.attached.remove(id);
        self.queued.remove(id);
        self.lapse_permissions(id)?;
        let notice = format!("connection to the agent closed: {error}");
        self.apply_update(id, &Update::Notice(notice))?;
        self.change_session(id.clone(), |closed| {
            if matches!(closed.status, Status::Running | Status::WaitingForYou) {
                closed.status = Status::Failed;
            }
        })
    }

    fn fail_session(&mut self, id: &SessionId, error: String) -> Result<(), AppError> {
        self.queued.remove(id);
        self.change_session(id.clone(), |session| session.status = Status::Failed)?;
        self.apply_update(id, &Update::Notice(error))
    }

    /// Sends `prompt` to the session's attached backend session, with the instruction chain
    /// (and an orchestrator's preamble after it) before the text when the session has not
    /// received it yet. Returns the instruction files that had to be skipped; the caller
    /// saves the session.
    fn send_prompt(
        &mut self,
        id: &SessionId,
        prompt: QueuedPrompt,
    ) -> Result<Vec<SkippedInstruction>, AppError> {
        let session = self.session(id)?;
        let (text, skipped) = if session.instructions_injected {
            (prompt.text, Vec::new())
        } else {
            let project = self.project(&session.project)?;
            let chain = session_instructions(project, session);
            let text = if session.orchestrator {
                format!("{}\n\n{}", preamble(project), prompt.text)
            } else {
                prompt.text
            };
            (inject(&chain, &text), chain.skipped)
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
            Update::PermissionRequested {
                id,
                title,
                kind,
                options,
                name,
                ..
            } => self.permission_requested(session, id, &title, kind, options, name.as_deref()),
            Update::TurnEnded(reason) => {
                self.end_turn(session, Status::Done, ChildOutcome::Ended(reason))
            }
            Update::Failed(error) => {
                self.end_turn(session, Status::Failed, ChildOutcome::Failed(error))
            }
            Update::ConfigOptions(options) => self.replace_config(session, options),
            _ => Ok(()),
        }
    }

    /// Records the request for the call `title` of `kind` from the tool `name` and answers it
    /// as [`apply_event`](Self::apply_event) describes, or leaves it waiting for the user.
    fn permission_requested(
        &mut self,
        session: &SessionId,
        permission: PermissionId,
        title: &str,
        kind: ToolKind,
        options: Vec<PermissionOption>,
        name: Option<&str>,
    ) -> Result<(), AppError> {
        if self.session(session)?.orchestrator {
            let partitur_tool = name.is_some_and(|name| name.starts_with(PARTITUR_TOOL_PREFIX));
            if partitur_tool && let Some(option) = allowing_option(&options) {
                self.permissions
                    .insert((session.clone(), permission), options);
                return self.answer_permission(session.clone(), permission, Some(option));
            }
            if !partitur_tool && let Some(word) = restrained(kind) {
                let rejection = rejecting_option(&options);
                self.permissions
                    .insert((session.clone(), permission), options);
                self.answer_permission(session.clone(), permission, rejection)?;
                return self.apply_update(session, &Update::Notice(denial_notice(word, title)));
            }
        }
        let automatic = if self.session(session)?.auto_allow {
            allowing_option(&options)
        } else {
            None
        };
        self.permissions
            .insert((session.clone(), permission), options);
        match automatic {
            Some(option) => self.answer_permission(session.clone(), permission, Some(option)),
            None => self.change_session(session.clone(), |waiting| {
                waiting.status = Status::WaitingForYou;
            }),
        }
    }

    /// Ends the session's turn with `status`: its unanswered permission requests lapse, a
    /// child reports `outcome` to its parent, and the next prompt Partitur deferred for the
    /// session goes out.
    fn end_turn(
        &mut self,
        session: &SessionId,
        status: Status,
        outcome: ChildOutcome,
    ) -> Result<(), AppError> {
        self.lapse_permissions(session)?;
        self.change_session(session.clone(), |ended| ended.status = status)?;
        let origin = self
            .turn_origins
            .remove(session)
            .unwrap_or(TurnOrigin::User);
        self.report_to_parent(session, outcome, origin)?;
        self.deliver_next(session)
    }

    /// Settles the session's waiting permission requests as cancelled, in the transcript and
    /// the store, without telling the backend: the agent can no longer receive the answers.
    fn lapse_permissions(&mut self, session: &SessionId) -> Result<(), AppError> {
        for (permission, _) in self.waiting_permissions(session) {
            self.permissions.remove(&(session.clone(), permission));
            let lapsed = Update::PermissionAnswered {
                id: permission,
                option: None,
            };
            self.apply_update(session, &lapsed)?;
        }
        Ok(())
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
    first_option(
        options,
        [PermissionKind::AllowOnce, PermissionKind::AllowAlways],
    )
}

/// The option an orchestrator denies a restrained call with: the first that rejects once,
/// else the first that rejects always.
fn rejecting_option(options: &[PermissionOption]) -> Option<PermissionOptionId> {
    first_option(
        options,
        [PermissionKind::RejectOnce, PermissionKind::RejectAlways],
    )
}

/// The first option of the first of `kinds` that `options` offers.
fn first_option(
    options: &[PermissionOption],
    kinds: [PermissionKind; 2],
) -> Option<PermissionOptionId> {
    kinds
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
