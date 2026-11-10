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

use crate::archive::Archived;
use crate::backend::{
    Backend, BackendError, BackendEvent, ChildOutcome, ConfigCategory, ConfigOption,
    PermissionKind, PermissionOption, PromptContent, ProposedChild, StopReason, ToolKind,
    TurnOrigin, Update,
};
use crate::ids::{
    BackendId, BackendInfo, ConfigValueId, PermissionId, PermissionOptionId, ProjectId, ProposalId,
    RepositoryId, RequestId, SessionId, ToolRequestId,
};
use crate::instructions::{InstructionChain, SkippedInstruction, inject, instruction_chain};
use crate::orchestrator::{
    PARTITUR_TOOL_PREFIX, denial_notice, is_orchestrator, preamble, restrained,
};
use crate::project::{Project, ProjectError, discover_project, rescan};
use crate::session::{Scope, Session, Status, title_from_prompt};
use crate::store::{Store, StoreError};
use crate::transcript::Transcript;
use crate::view_model::{EnvViewModel, SessionSummary, SessionViewModel, TreeModel};

mod draft;
mod orchestration;

use draft::Draft;
pub use orchestration::ChildChoice;
use orchestration::Generated;

/// The title of a session before its first prompt names it.
const NEW_SESSION_TITLE: &str = "New session";

/// The notice shown when a stored session could not be resumed and a fresh one replaced it.
const RESUMED_NOTICE: &str = "resumed without the agent's memory of earlier turns";

/// The answer to a tool call from a session that was deleted.
const DELETED_SESSION: &str = "the session was deleted";

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
    /// The session's loaded transcript has no undecided proposal with this id: it was
    /// decided already, or never made.
    #[error("no undecided proposal {0}")]
    UnknownProposal(ProposalId),
    /// The children chosen on an approval card cannot be started: there is none, one names
    /// a repository the project lacks, or one has a blank prompt.
    #[error("cannot start these children: {0}")]
    InvalidChildren(String),
    /// The session is archived: it takes no prompt and decides no proposal until it is
    /// restored, and archiving it again changes nothing.
    #[error("session {0} is archived; restore it first")]
    Archived(SessionId),
    /// The session is the child of an orchestrator, which archives and restores together
    /// with its children.
    #[error("archive the orchestrator: a child session archives with it")]
    ArchiveChild(SessionId),
    /// The session is the child of an orchestrator, which is deleted together with its
    /// children.
    #[error("delete the orchestrator: a child session is deleted with it")]
    DeleteChild(SessionId),
    /// The operation needs a session being prepared in the new-session sheet, and none is.
    #[error("no new session is being prepared")]
    NoDraft,
    /// The agent of the session being prepared could not start, so the session cannot be
    /// created on it.
    #[error("the agent did not start; pick another backend or cancel")]
    DraftFailed,
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
    /// The proposals that the user has not decided yet, with their children: the ones made in
    /// this process and the undecided ones of every transcript loaded from the store.
    proposals: HashMap<(SessionId, ProposalId), Vec<ProposedChild>>,
    /// The sessions deleted, and warm sessions discarded, in this process, whose late
    /// backend events are ignored.
    deleted: HashSet<SessionId>,
    /// The session being prepared in the new-session sheet, outside `sessions` and the store
    /// until it is created.
    draft: Option<Draft>,
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
            proposals: HashMap::new(),
            deleted: HashSet::new(),
            draft: None,
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
    /// The project counts as closed while its turns are stopped, so a child result they
    /// report waits in its orchestrator's queue instead of starting a turn. The project and
    /// its sessions stay saved, and opening the folder again restores them; closing deletes
    /// nothing.
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
        self.mark_closed(&project, true);
        for session in turns {
            match self.stop_turn(session) {
                Ok(()) | Err(AppError::Backend(BackendError::UnknownSession(_))) => {}
                Err(error) => {
                    self.mark_closed(&project, false);
                    return Err(error);
                }
            }
        }
        if let Some(current) = &self.current
            && self.session(current)?.project == project
        {
            self.current = None;
        }
        self.save_projects()
    }

    /// Archives `session` together with every session below it: they leave the live tree,
    /// keep their transcripts and statuses, and take no turn until they are restored.
    ///
    /// Each of them that is running or waiting is stopped like [`cancel`](Self::cancel)
    /// stops the current session, and the turn's end settles its status as usual; a session
    /// the backend no longer knows has no turn to stop. The family counts as archived while
    /// its turns are stopped, so a child result they report waits in its orchestrator's
    /// queue instead of starting a turn. All of them carry the same archive time. The
    /// current session stays selected.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::ArchiveChild`] when it is the child of an orchestrator,
    /// [`AppError::Archived`] when it is archived already (nothing changes in these cases),
    /// [`AppError::Backend`] when a turn cannot be stopped (the family stays live then, and
    /// a result held for it meanwhile goes out) and [`AppError::Store`] when the change
    /// cannot be saved.
    pub fn archive(&mut self, session: SessionId) -> Result<(), AppError> {
        let archived = self.session(&session)?;
        if archived.parent.is_some() {
            return Err(AppError::ArchiveChild(session));
        }
        if archived.archived.is_some() {
            return Err(AppError::Archived(session));
        }
        let project = archived.project.clone();
        let family = self.family(&session);
        let turns: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|member| {
                family.contains(&member.id)
                    && matches!(member.status, Status::Running | Status::WaitingForYou)
            })
            .map(|member| member.id.clone())
            .collect();
        self.mark_archived(&family, Some(Archived { at: unix_seconds() }));
        for turn in turns {
            match self.stop_turn(turn) {
                Ok(()) | Err(AppError::Backend(BackendError::UnknownSession(_))) => {}
                Err(error) => {
                    // The stop's error is the one to report; reviving can only fail on the
                    // same store or backend, which the next change runs into again.
                    let _also_failed = self.revive(&project, &family);
                    return Err(error);
                }
            }
        }
        self.save_sessions(&project)
    }

    /// Takes `family` of `project` live again after archiving it failed half-way: the
    /// sessions stopped so far may have saved the archive mark, so the list is saved again,
    /// and a result a stopped child reported while its orchestrator counted as archived goes
    /// out.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Store`] when the list cannot be saved and [`AppError::Backend`]
    /// when a held prompt cannot be sent.
    fn revive(&mut self, project: &ProjectId, family: &[SessionId]) -> Result<(), AppError> {
        self.mark_archived(family, None);
        self.save_sessions(project)?;
        self.deliver_waiting(family)
    }

    /// Sends the oldest prompt Partitur deferred for each session of `family` that is not in
    /// a turn; one still in a turn sends its deferred prompts when that turn ends.
    fn deliver_waiting(&mut self, family: &[SessionId]) -> Result<(), AppError> {
        for member in family {
            if !self.in_turn(member)? {
                self.deliver_next(member)?;
            }
        }
        Ok(())
    }

    /// Restores the archived `session` together with every session below it: they return
    /// to the live tree with the statuses they had, and the prompts Partitur deferred for
    /// them while they were archived go out, oldest first, unless their project is closed; a
    /// session whose stopped turn has not ended yet sends them when it ends. Restoring a live
    /// session changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::ArchiveChild`] when it is the child of an orchestrator (nothing changes
    /// in these cases), [`AppError::Backend`] when the backend cannot queue a deferred
    /// prompt and [`AppError::Store`] when the change cannot be saved.
    pub fn restore(&mut self, session: SessionId) -> Result<(), AppError> {
        let restored = self.session(&session)?;
        if restored.parent.is_some() {
            return Err(AppError::ArchiveChild(session));
        }
        if restored.archived.is_none() {
            return Ok(());
        }
        let project = restored.project.clone();
        let family = self.family(&session);
        self.mark_archived(&family, None);
        self.save_sessions(&project)?;
        self.deliver_waiting(&family)
    }

    /// Deletes `session` together with every session below it, live or archived, for good:
    /// they leave the tree and the store, their transcripts included, and their backend
    /// sessions are closed, which ends any turn they are in.
    ///
    /// The project's session list is saved without the family first. Then every member's
    /// backend session is closed and its transcript deleted; a failure there does not stop
    /// the others. Everything Partitur held for the family goes with it: waiting prompts,
    /// permission requests, undecided proposals and the prompts deferred for it, among them
    /// the results its children reported. A deleted current session is deselected. Whatever
    /// the backend still reports for a deleted session is ignored, and a tool call it makes
    /// is answered with `the session was deleted`.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::DeleteChild`] when it is the child of an orchestrator,
    /// [`AppError::Store`] when the session list cannot be saved (nothing changes in these
    /// cases), and after the family is gone the first [`AppError::Backend`] or
    /// [`AppError::Store`] error of closing a backend session or deleting a transcript.
    pub fn delete(&mut self, session: SessionId) -> Result<(), AppError> {
        let deleted = self.session(&session)?;
        if deleted.parent.is_some() {
            return Err(AppError::DeleteChild(session));
        }
        let project = deleted.project.clone();
        let family = self.family(&session);
        let remaining: Vec<Session> = self
            .sessions
            .iter()
            .filter(|kept| kept.project == project && !family.contains(&kept.id))
            .cloned()
            .collect();
        self.store.save_sessions(project, &remaining)?;
        self.forget(&family);
        let mut first_error = None;
        for member in family {
            let closed = self
                .backend
                .close_session(member.clone())
                .map_err(AppError::from);
            let removed = self.store.delete_updates(member).map_err(AppError::from);
            for failure in [closed, removed].into_iter().filter_map(Result::err) {
                first_error.get_or_insert(failure);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Drops `family` from memory and remembers it as deleted. A prompt generated from a
    /// member always waits for another member: a child's result for its parent, an
    /// orchestrator's prompt for its child.
    fn forget(&mut self, family: &[SessionId]) {
        let member = |session: &SessionId| family.contains(session);
        self.sessions.retain(|session| !member(&session.id));
        self.pending.retain(|_, (session, _)| !member(session));
        self.permissions.retain(|(session, _), _| !member(session));
        self.proposals.retain(|(session, _), _| !member(session));
        for session in family {
            self.transcripts.remove(session);
            self.config.remove(session);
            self.attached.remove(session);
            self.queued.remove(session);
            self.turn_origins.remove(session);
            self.deferred.remove(session);
        }
        if self.current.as_ref().is_some_and(member) {
            self.current = None;
        }
        self.deleted.extend(family.iter().cloned());
    }

    /// What the archive confirmation shows for `session`: its title and project, whether it
    /// is an orchestrator, how many sessions below it archive with it, where it stands and
    /// whether archiving stops a turn.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::ArchiveChild`] when it is the child of an orchestrator and
    /// [`AppError::Archived`] when it is archived already, as [`archive`](Self::archive)
    /// does.
    pub fn archive_summary(&self, session: &SessionId) -> Result<SessionSummary, AppError> {
        let summarized = self.session(session)?;
        if summarized.parent.is_some() {
            return Err(AppError::ArchiveChild(session.clone()));
        }
        if summarized.archived.is_some() {
            return Err(AppError::Archived(session.clone()));
        }
        self.summary(summarized)
    }

    /// What the delete confirmation shows for `session`, live or archived: the same as
    /// [`archive_summary`](Self::archive_summary) tells, with whether deleting stops a turn.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists and
    /// [`AppError::DeleteChild`] when it is the child of an orchestrator, as
    /// [`delete`](Self::delete) does.
    pub fn delete_summary(&self, session: &SessionId) -> Result<SessionSummary, AppError> {
        let summarized = self.session(session)?;
        if summarized.parent.is_some() {
            return Err(AppError::DeleteChild(session.clone()));
        }
        self.summary(summarized)
    }

    /// The summary of the top-level session `summarized` and the sessions below it.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownProject`] when its project does not exist.
    fn summary(&self, summarized: &Session) -> Result<SessionSummary, AppError> {
        let session = &summarized.id;
        let family = self.family(session);
        let active = |member: &SessionId| {
            self.session(member)
                .is_ok_and(|found| matches!(found.status, Status::Running | Status::WaitingForYou))
        };
        let below_is_active = family.iter().skip(1).any(active);
        Ok(SessionSummary {
            id: session.clone(),
            title: summarized.title.clone(),
            project: self.project(&summarized.project)?.name.clone(),
            orchestrator: summarized.orchestrator,
            children: family.len() - 1,
            status: if summarized.status == Status::Done && below_is_active {
                Status::Running
            } else {
                summarized.status
            },
            stops: family.iter().any(active),
        })
    }

    /// `session` and every session below it, `session` first.
    fn family(&self, session: &SessionId) -> Vec<SessionId> {
        let mut family = vec![session.clone()];
        for child in &self.sessions {
            if child.parent.as_ref() == Some(session) {
                family.extend(self.family(&child.id));
            }
        }
        family
    }

    /// Sets the archive mark of every session in `family` in memory; the caller saves.
    fn mark_archived(&mut self, family: &[SessionId], mark: Option<Archived>) {
        for member in &mut self.sessions {
            if family.contains(&member.id) {
                member.archived = mark;
            }
        }
    }

    /// Whether the session is archived.
    fn is_archived(&self, session: &SessionId) -> Result<bool, AppError> {
        Ok(self.session(session)?.archived.is_some())
    }

    /// Marks the project closed or open in memory; the caller saves the project list.
    fn mark_closed(&mut self, project: &ProjectId, closed: bool) {
        for marked in &mut self.projects {
            if &marked.id == project {
                marked.closed = closed;
            }
        }
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
        let backend = self.default.clone();
        let id = self.create_session(
            project,
            scope,
            None,
            NEW_SESSION_TITLE.into(),
            false,
            backend,
        )?;
        self.current = Some(id.clone());
        Ok(id)
    }

    /// Creates a session in `project` with `parent`, `title` and `auto_allow`, running on the
    /// configured backend `backend`, and asks the backend to start it, as
    /// [`new_session`](Self::new_session) describes, without selecting it.
    ///
    /// # Errors
    ///
    /// As [`new_session`](Self::new_session).
    fn create_session(
        &mut self,
        project: ProjectId,
        scope: Scope,
        parent: Option<SessionId>,
        title: String,
        auto_allow: bool,
        backend: BackendId,
    ) -> Result<SessionId, AppError> {
        let cwd = self.working_directory(&project, &scope)?;
        let orchestrator = is_orchestrator(self.project(&project)?, &scope);
        let session = Session {
            id: SessionId::new(),
            project,
            scope,
            cwd,
            title,
            backend_session: None,
            backend,
            model: None,
            effort: None,
            status: Status::Idle,
            parent,
            auto_allow,
            instructions_injected: false,
            created_at: unix_seconds(),
            orchestrator,
            archived: None,
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
        self.load_transcript(&session)?;
        self.current = Some(session);
        Ok(())
    }

    /// Loads the session's transcript from the store unless it is loaded already. A
    /// permission request it holds that Partitur is not waiting on was left unanswered by a
    /// quit, so it is settled as cancelled in the transcript and the store: the agent that
    /// asked is gone, and an open request would keep the session in a turn it is not in. Its
    /// undecided proposals join the ones the user has not decided, so a proposal made before
    /// a restart still holds its orchestrator.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists and
    /// [`AppError::Store`] when its updates cannot be loaded or a lapsed request cannot be
    /// saved; the transcript stays unloaded then.
    fn load_transcript(&mut self, id: &SessionId) -> Result<(), AppError> {
        self.session(id)?;
        if self.transcripts.contains_key(id) {
            return Ok(());
        }
        let mut transcript = Transcript::from_updates(&self.store.load_updates(id.clone())?);
        let stale: Vec<PermissionId> = transcript
            .unanswered_permissions()
            .filter(|permission| !self.permissions.contains_key(&(id.clone(), *permission)))
            .collect();
        for permission in stale {
            let lapsed = Update::PermissionAnswered {
                id: permission,
                option: None,
            };
            self.store.append_update(id.clone(), &lapsed)?;
            transcript.apply(&lapsed);
        }
        for (proposal, children) in transcript.pending_proposals() {
            self.proposals
                .insert((id.clone(), proposal.clone()), children.to_vec());
        }
        self.transcripts.insert(id.clone(), transcript);
        Ok(())
    }

    /// The selected session, if any.
    pub fn current_session(&self) -> Option<SessionId> {
        self.current.clone()
    }

    /// Builds the navigation tree: open projects in the order they were opened, each with
    /// its sessions newest first (children nested under their parent) and its repositories
    /// as discovered. Sessions created within the same second count the later one as newer.
    /// A session's context usage is known once its transcript is loaded: after it was
    /// created or selected in this run.
    pub fn tree(&self) -> TreeModel {
        TreeModel::build(&self.projects, &self.sessions, &self.transcripts)
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
    /// [`AppError::Archived`] when the session is archived,
    /// [`AppError::UnknownBackend`] when the session's backend is no longer configured,
    /// [`AppError::TurnInProgress`] when the session is [`Running`](Status::Running) or a
    /// permission request of its turn waits for an answer (an orchestrator that waits for you
    /// only for the decision on its proposal takes the prompt),
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
    /// assert!(view.running());
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
        if session.archived.is_some() {
            return Err(AppError::Archived(id));
        }
        if !self.is_configured(&session.backend) {
            return Err(AppError::UnknownBackend(session.backend.clone()));
        }
        if self.in_turn(&id)? {
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

    /// Stops the turn of every child session of `session` that is in a turn, each as
    /// [`cancel`](Self::cancel) stops the current session's: a running child is cancelled, a
    /// child that waits for the answer to a permission request has the request cancelled
    /// first and then its turn, and a child whose prompt has not gone out yet drops it. A
    /// child that is idle, done or failed is left alone, and so are the sessions below the
    /// children and `session` itself. Each stopped child reports its turn to `session` as
    /// cancelled.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists (nothing changes
    /// then), [`AppError::Backend`] when the backend cannot queue a command and
    /// [`AppError::Store`] when a change cannot be saved. A failure ends the call there: the
    /// children stopped before it stay stopped, the others keep running.
    pub fn stop_children(&mut self, session: &SessionId) -> Result<(), AppError> {
        self.session(session)?;
        let children: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|child| child.parent.as_ref() == Some(session))
            .map(|child| child.id.clone())
            .collect();
        for child in children {
            if self.in_turn(&child)? {
                self.stop_turn(child)?;
            }
        }
        Ok(())
    }

    /// Stops the session's turn as [`cancel`](Self::cancel) describes. A child whose prompt
    /// is dropped before it went out reports its turn to its parent as cancelled; prompts
    /// Partitur deferred for the session stay queued.
    fn stop_turn(&mut self, id: SessionId) -> Result<(), AppError> {
        if self.queued.remove(&id).is_some() {
            self.change_session(id.clone(), |session| session.status = Status::Idle)?;
            return self.settle_turn(&id, ChildOutcome::Ended(StopReason::Cancelled), false);
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
    /// [`AppError::TurnInProgress`] when the session is [`Running`](Status::Running) or a
    /// permission request of its turn waits for an answer (an orchestrator that waits for you
    /// only for the decision on its proposal can switch), [`AppError::Store`] when the session's
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
    /// assert!(!view.running());
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
        if self.in_turn(&session)? {
            return Err(AppError::TurnInProgress);
        }
        if self.session(&session)?.backend == backend {
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
            &self.transcripts,
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
        let transcript = self.transcripts.get(&session.id)?;
        Some(EnvViewModel::build(
            project,
            &self.sessions,
            session,
            session_instructions(project, session),
            transcript,
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
    /// A turn the agent starts without a prompt makes the session
    /// [`Running`](Status::Running) and is recorded in the transcript; the same signal while
    /// the session is already in a turn is ignored, since that turn is already running.
    ///
    /// An update or a close for a session that is not attached comes from a connection the
    /// session no longer uses (it switched backends, or its connection already closed) and
    /// is ignored.
    ///
    /// Any event for a session deleted, or a warm session discarded, in this process is
    /// ignored; a tool call it makes is answered with `the session was deleted`. An event for
    /// the warm session of the [session being prepared](Self::prepare_session) updates the
    /// draft instead: the answer to its create makes it ready or failed, a close fails it, new
    /// config options replace its options once it is ready, any other update is held until
    /// the session is created, and a tool call is answered with `unknown session`.
    ///
    /// [`BackendEvent::ToolCalled`] is answered at once: `propose_children` records the
    /// proposal and `send_to_child` prompts the child (or queues the prompt while the child is
    /// busy) and records the prompt in the orchestrator's transcript; a call that is not valid
    /// is answered with the reason and changes nothing. An
    /// orchestrator whose turn ends while one of its proposals is undecided waits for you
    /// instead of being done.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when the event names a session that does not
    /// exist, [`AppError::Backend`] when a follow-up command cannot be queued and
    /// [`AppError::Store`] when the change cannot be saved.
    pub fn apply_event(&mut self, event: BackendEvent) -> Result<(), AppError> {
        if self.deleted.contains(event_session(&event)) {
            return self.ignore_deleted(&event);
        }
        if self.is_draft(event_session(&event)) {
            return self.draft_event(event);
        }
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
                if update == Update::TurnStarted && self.in_turn(&session)? {
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
            BackendEvent::ToolCalled {
                call,
                session,
                tool,
            } => self.tool_called(call, &session, tool),
        }
    }

    /// Drops `event`, which names a deleted session; a tool call is answered so the agent
    /// does not wait for an answer that never comes, unless nothing waits for it any more.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Backend`] when the answer cannot be queued.
    fn ignore_deleted(&self, event: &BackendEvent) -> Result<(), AppError> {
        let BackendEvent::ToolCalled { call, .. } = event else {
            return Ok(());
        };
        self.refuse_tool(*call, DELETED_SESSION)
    }

    /// Answers the tool call `call` with the error `reason`, so the agent does not wait for
    /// an answer that never comes; a call nothing waits for any more needs no answer.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Backend`] when the answer cannot be queued.
    fn refuse_tool(&self, call: ToolRequestId, reason: &str) -> Result<(), AppError> {
        match self.backend.answer_tool(call, Err(reason.to_owned())) {
            Ok(()) | Err(BackendError::UnknownToolCall(_)) => Ok(()),
            Err(error) => Err(error.into()),
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
    /// are stored, the model and effort it recorded are re-applied as
    /// [`reapply_choices`](Self::reapply_choices) describes, and its waiting prompts are sent.
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
    /// list the value among their choices and report another current value. A recorded value
    /// the option does not list is replaced by the option's current value in memory, without
    /// telling the backend; the caller saves. A category the backend does not offer is left
    /// alone.
    fn reapply_choices(&mut self, session: &SessionId) -> Result<(), AppError> {
        let recorded = self.session(session)?;
        let (model, effort) = (recorded.model.clone(), recorded.effort.clone());
        let model = self.reapplied(session, ConfigCategory::Model, model)?;
        let effort = self.reapplied(session, ConfigCategory::Effort, effort)?;
        let reapplied = self.session_mut(session)?;
        reapplied.model = model;
        reapplied.effort = effort;
        Ok(())
    }

    /// Re-applies the recorded `value` of `category` as
    /// [`reapply_choices`](Self::reapply_choices) describes and returns the value the session
    /// records from now on.
    fn reapplied(
        &mut self,
        session: &SessionId,
        category: ConfigCategory,
        value: Option<ConfigValueId>,
    ) -> Result<Option<ConfigValueId>, AppError> {
        let Some(value) = value else { return Ok(None) };
        let Some(option) = self
            .config_of(session)
            .iter()
            .find(|option| option.category == category)
        else {
            return Ok(Some(value));
        };
        if !option.choices.iter().any(|choice| choice.id == value) {
            return Ok(Some(option.current.clone()));
        }
        if option.current != value {
            self.set_config(session, category, &value)?;
        }
        Ok(Some(value))
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
    ///
    /// A turn cut short this way ends like a failed one: a child reports the notice to its
    /// parent as the failure, and the next prompt Partitur deferred for the session goes out,
    /// which loads the backend session again.
    fn close_session(&mut self, id: &SessionId, error: &str) -> Result<(), AppError> {
        let in_turn = self.in_turn(id)?;
        self.attached.remove(id);
        self.queued.remove(id);
        self.lapse_permissions(id)?;
        let notice = format!("connection to the agent closed: {error}");
        self.apply_update(id, &Update::Notice(notice.clone()))?;
        if !in_turn {
            return Ok(());
        }
        self.change_session(id.clone(), |closed| closed.status = Status::Failed)?;
        self.settle_turn(id, ChildOutcome::Failed(notice), true)
    }

    /// Fails a session whose backend session could not be created: its waiting prompts are
    /// dropped and a notice says why. A child that was in a turn reports `error` to its
    /// parent as the failure; prompts Partitur deferred for the session stay queued, since a
    /// session that cannot be created would only fail them again.
    fn fail_session(&mut self, id: &SessionId, error: String) -> Result<(), AppError> {
        let in_turn = self.in_turn(id)?;
        self.queued.remove(id);
        self.change_session(id.clone(), |session| session.status = Status::Failed)?;
        self.apply_update(id, &Update::Notice(error.clone()))?;
        if !in_turn {
            return Ok(());
        }
        self.settle_turn(id, ChildOutcome::Failed(error), false)
    }

    /// Whether the session is in a turn: [`Running`](Status::Running), or
    /// [`WaitingForYou`](Status::WaitingForYou) with a permission request of its turn waiting
    /// for an answer. An orchestrator that waits for you only for the decision on its proposal
    /// is not in a turn.
    fn in_turn(&self, id: &SessionId) -> Result<bool, AppError> {
        Ok(match self.session(id)?.status {
            Status::Running => true,
            Status::WaitingForYou => self.permissions.keys().any(|(waiting, _)| waiting == id),
            Status::Idle | Status::Done | Status::Failed => false,
        })
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
            Update::TurnStarted => {
                self.turn_origins.insert(session.clone(), TurnOrigin::Agent);
                self.change_session(session.clone(), |started| started.status = Status::Running)
            }
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
    /// session goes out. A cancelled turn reports too but sends nothing: a stopped session
    /// stays quiet until the user prompts it, and its queue moves on when a later turn ends
    /// otherwise. A session that would be [`Done`](Status::Done) with an undecided proposal
    /// is [`WaitingForYou`](Status::WaitingForYou) instead: it waits for the decision, but its
    /// turn is over, so it takes a user prompt and its next deferred prompt goes out as after
    /// any other turn; the turn that starts ends in [`WaitingForYou`](Status::WaitingForYou)
    /// again while the proposal is undecided. A turn the agent started on its own ends the
    /// same way and reports to the parent as started by the agent.
    fn end_turn(
        &mut self,
        session: &SessionId,
        status: Status,
        outcome: ChildOutcome,
    ) -> Result<(), AppError> {
        let status = if status == Status::Done && self.awaits_decision(session) {
            Status::WaitingForYou
        } else {
            status
        };
        self.lapse_permissions(session)?;
        self.change_session(session.clone(), |ended| ended.status = status)?;
        let drain = outcome != ChildOutcome::Ended(StopReason::Cancelled);
        self.settle_turn(session, outcome, drain)
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

/// The session `event` names.
fn event_session(event: &BackendEvent) -> &SessionId {
    match event {
        BackendEvent::SessionCreated { session, .. }
        | BackendEvent::SessionLoaded { session, .. }
        | BackendEvent::Update { session, .. }
        | BackendEvent::SessionClosed { session, .. }
        | BackendEvent::RequestFailed { session, .. }
        | BackendEvent::ToolCalled { session, .. } => session,
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
