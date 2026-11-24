//! [`App`], the core's single entry point for the UI.
//!
//! The app is a synchronous struct behind its ports: a [`Backend`] that runs the agents, a
//! [`Store`] it loads the saved state from at start, a [`StoreQueue`] every later write goes
//! through without waiting, a [`Git`] port that asks and changes the project's repositories,
//! and a [`Files`] port every later read of the file system goes through, such as the scan of
//! a folder opened as a project. The UI calls its methods, feeds it the backend's
//! [`BackendEvent`]s, the git port's [`GitEvent`]s, the store queue's [`StoreEvent`]s and the
//! files port's [`FilesEvent`](crate::files::FilesEvent)s, and renders the view-models it
//! returns. A write the store fails is kept in memory all the same; its failure is reported
//! when its event arrives.
//!
//! A session is titled `untitled` until the agent titles it. The prompt of its first turn,
//! typed by the user or given by its orchestrator, is kept whole together with the skill it
//! starts with, and does not change the title; a later prompt never replaces it.
//!
//! A child's permission request stays the user's to answer, never its orchestrator's. It
//! waits in the child's own session and, until it is settled, in its orchestrator's too.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use thiserror::Error;

use crate::archive::Archived;
use crate::backend::{
    AgentUpdate, Backend, BackendError, BackendEvent, ChildOutcome, ConfigCategory, ConfigOption,
    PermissionKind, PermissionOption, PromptContent, SentAttachment, StopReason, ToolKind,
    TurnOrigin,
};
use crate::clock::Clock;
use crate::files::{Files, FilesError};
use crate::git::{Git, GitError, GitEvent, GitStep, Head};
use crate::git_card::{Boundary, outside_paths};
use crate::history_marks::HistoryMarks;
use crate::ids::{
    BackendId, BackendInfo, ConfigId, ConfigValueId, PermissionId, PermissionOptionId, ProjectId,
    ProposalId, RepositoryId, RequestId, SessionId,
};
use crate::instructions::{InstructionChain, SkippedInstruction, inject, instruction_chain};
use crate::link_opener::LinkOpener;
use crate::orchestrator::{
    denial_notice, is_orchestrator, preamble, reads_only_in, restrained,
    writes_only_in_workflow_folders,
};
use crate::project::{Project, Repository};
use crate::question::QuestionOutcome;
use crate::record::TranscriptRecord;
use crate::session::{Role, Scope, Session, Status, TitleSource, Workspace, split_skill};
use crate::settings::{GlobalSettings, ProjectSettings};
use crate::store::{LoadId, Store, StoreCommand, StoreError, StoreEvent, StoreQueue};
use crate::tool_call::ToolCall;
use crate::transcript::Transcript;
use crate::view_model::{
    ChildPermission, EnvViewModel, Live, SessionSummary, SessionViewModel, TreeModel, at_work,
    scope_label,
};
use crate::workflow::{Closing, Decider, GateCloser};

mod cleanup;
mod draft;
mod env_git;
mod git_work;
mod live;
mod opening;
mod peers;
mod question;
mod scheduler;
mod split;
#[cfg(test)]
pub(crate) mod test_support;
mod titles;
mod tools;
mod workflow;
mod workspace;

pub use cleanup::Cleanup;
pub use draft::Created;
use draft::Draft;
use git_work::{GitWork, Work};
use live::{LiveSession, Permissions, Requests, TranscriptState, owned};
use peers::PeerRead;
use split::SplitState;
pub use split::{Approval, ChildChoice};
use workspace::undo_steps;

/// The title of a session until the agent titles it.
const NEW_SESSION_TITLE: &str = "untitled";

/// What a new session starts with besides its place in the project: the backend it runs on,
/// the model and effort it asks for once its agent answers, whether it grants permissions
/// without asking and where its title comes from.
struct SessionChoices {
    backend: BackendId,
    model: Option<ConfigValueId>,
    effort: Option<ConfigValueId>,
    auto_allow: bool,
    title_source: TitleSource,
}

/// What the views of a session show while its transcript loads.
static NOT_LOADED: LazyLock<Transcript> = LazyLock::new(Transcript::default);

/// The notice shown when a stored session could not be resumed and a fresh one replaced it.
const RESUMED_NOTICE: &str = "resumed without the agent's memory of earlier turns";

/// The notice a session gets when Partitur closed during its turn, so the turn never ended.
const INTERRUPTED: &str = "Partitur closed before this turn ended";

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
    /// A project folder could not be scanned.
    #[error(transparent)]
    Files(#[from] FilesError),
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
    /// The session has no design, plan or checkpoint waiting for the user's approval.
    #[error("session {0} has no gate waiting for approval")]
    NoGate(SessionId),
    /// The session has no question waiting for the user's answer.
    #[error("session {0} has no question waiting for an answer")]
    NoQuestion(SessionId),
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
    /// The git port could not queue a request.
    #[error(transparent)]
    Git(#[from] GitError),
    /// The branch settings picked in the new-session sheet cannot be used.
    #[error("cannot use these branch settings: {0}")]
    Workspace(String),
    /// What a create made could not be undone, so a worktree or branch it made may be left
    /// in the repository.
    #[error("could not undo the branch work in {repository}: {error}")]
    Undo {
        /// The repository's name.
        repository: String,
        /// Why the undo could not be queued or failed.
        error: GitError,
    },
    /// A worktree or branch checked for removal when its session was deleted could not be
    /// removed.
    #[error("could not remove the worktree or branch in {repository}: {error}")]
    Cleanup {
        /// The repository's name.
        repository: String,
        /// Why the removal could not be queued or failed.
        error: GitError,
    },
    /// A worktree or branch checked for removal when its session was deleted was kept,
    /// because the repository could not be read again or it no longer is what the delete
    /// sheet showed.
    #[error("kept the worktree or branch in {repository}: {reason}")]
    CleanupKept {
        /// The repository's name.
        repository: String,
        /// What changed, one reason per kept worktree or branch.
        reason: String,
    },
}

/// One attachment of a prompt that was read and copied in the background: the content the
/// agent is sent and the record the transcript keeps of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    /// What the agent is sent.
    pub content: PromptContent,
    /// What the transcript shows of it afterwards.
    pub sent: SentAttachment,
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
/// Owns its [`Backend`], [`Store`], [`StoreQueue`], [`Git`] and [`Files`] port. Backend
/// outcomes reach it through [`apply_event`](Self::apply_event), git outcomes through
/// [`apply_git_event`](Self::apply_git_event), the store queue's through
/// [`apply_store_event`](Self::apply_store_event) and the files port's through
/// [`apply_files_event`](Self::apply_files_event).
pub struct App {
    /// Every write after start-up, and loads that must see them. Declared first, so it is
    /// dropped first: a queue that finishes its queued writes when dropped has done so before
    /// the backend shuts down.
    queue: Box<dyn StoreQueue>,
    backend: Box<dyn Backend>,
    /// What start-up loads, and the reads that are not queued yet.
    store: Box<dyn Store>,
    git: Box<dyn Git>,
    /// Every read of the file system after start-up.
    files: Box<dyn Files>,
    /// The folders being opened as projects, in the order they were picked.
    opening: Vec<opening::Opening>,
    /// The number of the latest request to `files`.
    next_files: u64,
    /// The time, for every stamp the app writes.
    clock: Box<dyn Clock>,
    /// The `read_session` calls whose loads are queued, by load; their answers are not kept.
    reads: HashMap<LoadId, PeerRead>,
    /// The id of the latest load queued.
    next_load: u64,
    /// The global settings layer from `config.toml` and the home folder a worktree path's
    /// leading `~` expands to.
    global: GlobalSettings,
    /// The git requests queued and not answered yet, with what each was made for.
    git_work: GitWork,
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
    /// What this process keeps about each session beyond what is stored; a session without
    /// an entry has no transcript loaded and nothing else kept.
    live: HashMap<SessionId, LiveSession>,
    /// Every project's `settings.toml`, read once when the project was loaded or opened. An
    /// unreadable or invalid file is kept as the error's text, which every use reports.
    settings: HashMap<ProjectId, Result<ProjectSettings, String>>,
    /// Backend requests that have not been answered yet.
    requests: Requests,
    /// Permission requests waiting for the user's answer.
    permissions: Permissions,
    /// The approval card state of undecided proposals: what git answered about their
    /// children's repositories, and the approval in flight or why the last one failed.
    splits: HashMap<ProposalId, SplitState>,
    /// The sessions deleted, and warm sessions discarded, in this process, whose late
    /// backend events are ignored.
    deleted: HashSet<SessionId>,
    /// The session being prepared in the new-session sheet, outside `sessions` and the store
    /// until it is created.
    draft: Option<Draft>,
    /// Whether the UI shows the Env tab, so a turn's end reads git again for its card.
    env_open: bool,
    /// The temporary folder as the environment names it and resolved, once at load, so the
    /// Git card never resolves it while the UI waits.
    temp_dirs: Vec<PathBuf>,
    next_request: u64,
    current: Option<SessionId>,
}

impl App {
    /// Restores the projects and sessions the store holds, closed projects included; only
    /// open ones appear in the [`tree`](Self::tree).
    ///
    /// `backends` are the configured backends and `default` the one new sessions run on. A
    /// stored session saved before sessions named their backend is given `default`. `git` is
    /// the port every git request goes through, `files` the port every later read of the file
    /// system goes through, and `global` the settings from `config.toml` that every project's
    /// settings layer over.
    ///
    /// A session cannot be running after a restart. One stored as running, or as waiting on a
    /// permission request, was in a turn when Partitur closed, and that turn never ended: it
    /// becomes [`Interrupted`](Status::Interrupted) and its transcript gets a notice saying so.
    /// One that was interrupted before stays so. Any other session whose stored workflow view
    /// has a gate open, or that has a question open, still waits for the user and starts
    /// [`WaitingForYou`](Status::WaitingForYou); what is stored with the session decides this,
    /// without reading its history. Every other session starts
    /// [`Idle`](Status::Idle). A project whose sessions changed this way has its session list
    /// saved once, so a later restart neither migrates nor notes them again. The backend is
    /// not asked to resume anything yet.
    ///
    /// The children that wait for their orchestrator are reported to it only by
    /// [`report_waiting_at_start`](Self::report_waiting_at_start), which the UI calls once
    /// the app is loaded.
    ///
    /// `clock` is where the app takes every time it stamps from.
    ///
    /// Everything above is read and written through `store` before this returns. Every
    /// write after that goes through `queue` and does not wait for the store: its failure
    /// arrives later through [`apply_store_event`](Self::apply_store_event).
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoBackends`] when `backends` is empty or does not contain
    /// `default`, and [`AppError::Store`] when the projects or their sessions cannot be
    /// loaded, an interrupted session's notice cannot be saved, or a changed session list
    /// cannot be saved.
    #[expect(
        clippy::too_many_arguments,
        reason = "each port of the app is handed over on its own"
    )]
    pub fn load(
        backend: Box<dyn Backend>,
        store: Box<dyn Store>,
        queue: Box<dyn StoreQueue>,
        git: Box<dyn Git>,
        files: Box<dyn Files>,
        backends: Vec<BackendInfo>,
        default: BackendId,
        global: GlobalSettings,
        clock: Box<dyn Clock>,
    ) -> Result<Self, AppError> {
        if !backends.iter().any(|info| info.id == default) {
            return Err(AppError::NoBackends);
        }
        let projects = store.load_projects()?;
        let mut sessions = Vec::new();
        for project in &projects {
            let mut loaded = store.load_sessions(project.id.clone())?;
            let mut changed = false;
            for session in &mut loaded {
                if session.backend.is_unset() {
                    session.backend = default.clone();
                    changed = true;
                }
                session.status = if session.status == Status::Interrupted {
                    Status::Interrupted
                } else if left_in_turn(store.as_ref(), session)? {
                    let notice = TranscriptRecord::Notice(INTERRUPTED.into());
                    store.append_record(session.project.clone(), session.id.clone(), &notice)?;
                    if let Some(marks) = &mut session.history {
                        marks.apply(&notice);
                    }
                    changed = true;
                    Status::Interrupted
                } else if session
                    .workflow
                    .as_ref()
                    .is_some_and(|view| view.gate.is_some())
                    || session.question.is_some()
                {
                    Status::WaitingForYou
                } else {
                    Status::Idle
                };
            }
            if changed {
                store.save_sessions(project.id.clone(), &loaded)?;
            }
            sessions.extend(loaded);
        }
        let settings = projects
            .iter()
            .map(|project| (project.id.clone(), read_settings(&*store, &project.id)))
            .collect();
        Ok(Self {
            queue,
            backend,
            store,
            git,
            files,
            opening: Vec::new(),
            next_files: 0,
            clock,
            reads: HashMap::new(),
            next_load: 0,
            global,
            git_work: GitWork::default(),
            backends,
            default,
            projects,
            sessions,
            live: HashMap::new(),
            settings,
            requests: Requests::default(),
            permissions: Permissions::default(),
            splits: HashMap::new(),
            deleted: HashSet::new(),
            draft: None,
            env_open: false,
            temp_dirs: env_git::both_spellings(std::env::temp_dir()),
            next_request: 0,
            current: None,
        })
    }

    /// Applies what the store queue reported. A failed write becomes the
    /// [`AppError::Store`] a failed save has always been, for the UI to show; what it would
    /// have saved stays as it is in memory. A load answers the `read_session` call it was
    /// made for, or brings in the session's transcript, and what waited for it runs then.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Store`] for a [`StoreEvent::Failed`] and for a transcript that
    /// could not be loaded, and the first error of what waited for a loaded transcript.
    pub fn apply_store_event(&mut self, event: StoreEvent) -> Result<(), AppError> {
        match event {
            StoreEvent::Failed { error, .. } => Err(AppError::Store(owned(error))),
            StoreEvent::Loaded {
                request,
                session,
                records,
            } => match self.reads.remove(&request) {
                Some(read) => {
                    self.answer_read(&session, read, records);
                    Ok(())
                }
                None => self.transcript_loaded(request, &session, records),
            },
        }
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
    /// Returns [`AppError::UnknownProject`] when no open project has this id and
    /// [`AppError::Backend`] when a turn cannot be stopped (the project stays open then).
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
        self.save_projects();
        Ok(())
    }

    /// Archives `session` together with every session below it: they leave the live tree,
    /// keep their transcripts and statuses, and take no turn until they are restored.
    ///
    /// Each of them that is running or waiting, or whose background tasks still run, is stopped like
    /// [`cancel`](Self::cancel) stops the current session, and the turn's end settles its
    /// status as usual; a session
    /// the backend no longer knows has no turn to stop. The family counts as archived while
    /// its turns are stopped, so a child result they report waits in its orchestrator's
    /// queue instead of starting a turn. All of them carry the same archive time. The
    /// current session stays selected.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists, [`AppError::ArchiveChild`]
    /// when it is the child of an orchestrator, [`AppError::Archived`] when it is archived already
    /// (nothing changes in these cases) and [`AppError::Backend`] when a turn cannot be stopped
    /// (the family stays live then, and a result held for it meanwhile goes out).
    pub fn archive(&mut self, session: SessionId) -> Result<(), AppError> {
        let archived = self.session(&session)?;
        if archived.role.parent().is_some() {
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
                    && at_work(member.status, self.background_of(&member.id))
            })
            .map(|member| member.id.clone())
            .collect();
        self.mark_archived(
            &family,
            Some(Archived {
                at: self.clock.now(),
            }),
        );
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
        self.save_sessions(&project);
        Ok(())
    }

    /// Takes `family` of `project` live again after archiving it failed half-way: the
    /// sessions stopped so far may have saved the archive mark, so the list is saved again,
    /// and a result a stopped child reported while its orchestrator counted as archived goes
    /// out.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Backend`] when a held prompt cannot be sent.
    fn revive(&mut self, project: &ProjectId, family: &[SessionId]) -> Result<(), AppError> {
        self.mark_archived(family, None);
        self.save_sessions(project);
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
    /// session whose stopped turn has not ended yet sends them when it ends. A restored
    /// orchestrator then hears of the children that wait for a decision, which they held for
    /// the user while it was archived, as
    /// [`report_waiting_children`](Self::report_waiting_children) describes. Restoring a live
    /// session changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists, [`AppError::ArchiveChild`]
    /// when it is the child of an orchestrator (nothing changes in these cases),
    /// [`AppError::Backend`] when the backend cannot queue a deferred prompt or a waiting child's
    /// report and [`AppError::Store`] when a waiting child's transcript cannot be loaded.
    pub fn restore(&mut self, session: SessionId) -> Result<(), AppError> {
        let restored = self.session(&session)?;
        if restored.role.parent().is_some() {
            return Err(AppError::ArchiveChild(session));
        }
        if restored.archived.is_none() {
            return Ok(());
        }
        let project = restored.project.clone();
        let family = self.family(&session);
        self.mark_archived(&family, None);
        self.save_sessions(&project);
        if self
            .current
            .as_ref()
            .is_some_and(|shown| family.contains(shown))
            && self.env_open
        {
            self.check_env_git();
        }
        self.deliver_waiting(&family)?;
        self.report_waiting_children(&session)
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
    /// Once the family is gone, what the user checked in `cleanup` is removed from git in the
    /// background, by force (`git worktree remove --force`, `git branch -D`): only boxes of a
    /// line the latest [`delete_summary`](Self::delete_summary) offered, once git answered
    /// it and while its repository is not busy, so a line still loading or busy removes
    /// nothing (what is checked on a busy line is reported as kept), and neither do
    /// unchecked boxes. A branch checked out elsewhere is never deleted,
    /// and a Worktree session's branch only together with its worktree, unless git no
    /// longer lists that worktree. Each repository with something to remove is read again
    /// first and stays busy from now on until its removal has run, as one request holding
    /// every checked step of the family there in order, each worktree before its branch. A
    /// worktree or branch that no longer is what the sheet showed is kept: a worktree git no
    /// longer lists, that has something else checked out than the sheet showed or another
    /// count of uncommitted files than the sheet showed, a branch whose worktree is kept,
    /// that is gone or checked out elsewhere, and one the sheet showed merged that now is
    /// not. Two limits remain: a branch the sheet showed as not merged is deleted even when
    /// more commits arrived on it since, as git is not asked for its tip, and the agent of a
    /// running session may still write into its worktree between the fresh reading and the
    /// removal, since closing its backend session is only queued. The project's
    /// [`cleaning`](crate::ProjectNode::cleaning) is set until every removal has answered.
    /// Kept items and failed removals are reported by
    /// [`apply_git_event`](Self::apply_git_event).
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists, [`AppError::DeleteChild`]
    /// when it is the child of an orchestrator (nothing changes in these cases), and after the
    /// family is gone the first [`AppError::Backend`] error of closing a backend session,
    /// [`AppError::CleanupKept`] when a checked line's repository is busy, or [`AppError::Cleanup`]
    /// when the git port cannot queue a removal's reading.
    pub fn delete(&mut self, session: SessionId, cleanup: &[Cleanup]) -> Result<(), AppError> {
        let deleted = self.session(&session)?;
        if deleted.role.parent().is_some() {
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
        self.queue.send(StoreCommand::SaveSessions {
            project: project.clone(),
            sessions: remaining,
        });
        let sheet = self.live_mut(&session).and_then(|live| live.cleanup.take());
        self.forget(&family);
        let mut first_error = None;
        for member in family {
            if let Err(failure) = self.backend.close_session(member.clone()) {
                first_error.get_or_insert(AppError::from(failure));
            }
            self.queue.send(StoreCommand::Delete {
                project: project.clone(),
                session: member,
            });
        }
        if let Some(sheet) = sheet {
            for failure in self.remove_checked(&project, &sheet, cleanup) {
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
        self.splits.retain(|_, split| !member(split.session()));
        for session in family {
            self.forget_live(session);
        }
        if self.current.as_ref().is_some_and(member) {
            self.current = None;
        }
        self.deleted.extend(family.iter().cloned());
    }

    /// Whether anything `App` keeps per session still mentions `session`: the session list,
    /// its live entry, the requests, permissions and splits made for it, the draft whose warm
    /// session it is, and the shown session. `deleted` is left out on purpose: a forgotten
    /// session stays in it, so its late events, loads and reads are refused.
    #[cfg(test)]
    pub(crate) fn holds_anything_for(&self, session: &SessionId) -> bool {
        self.sessions.iter().any(|kept| &kept.id == session)
            || self.live.contains_key(session)
            || self.requests.any_for(session)
            || self.permissions.holds(session)
            || self.splits.values().any(|split| split.session() == session)
            || self.is_draft(session)
            || self.current.as_ref() == Some(session)
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
        if summarized.role.parent().is_some() {
            return Err(AppError::ArchiveChild(session.clone()));
        }
        if summarized.archived.is_some() {
            return Err(AppError::Archived(session.clone()));
        }
        self.summary(summarized)
    }

    /// What the delete confirmation shows for `session`, live or archived: the same as
    /// [`archive_summary`](Self::archive_summary) tells, with whether deleting stops a turn,
    /// and what deleting can also remove from git.
    ///
    /// The [`cleanup`](SessionSummary::cleanup) lines are one per Worktree or New branch
    /// session of the family, in family order, all loading at first; a Checked-out session
    /// has none. One read-only git request asks, in the background, for each line's
    /// repository, its worktree's uncommitted files and whether its branch is merged into the
    /// base it started from; [`cleanup`](Self::cleanup) has the lines as far as git has
    /// answered, through [`apply_git_event`](Self::apply_git_event). A repository git cannot
    /// read offers nothing, and so does every line when the request cannot be queued. Each
    /// call reads again, and an answer for an earlier call is ignored.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists and
    /// [`AppError::DeleteChild`] when it is the child of an orchestrator, as
    /// [`delete`](Self::delete) does.
    pub fn delete_summary(&mut self, session: &SessionId) -> Result<SessionSummary, AppError> {
        let summarized = self.session(session)?;
        if summarized.role.parent().is_some() {
            return Err(AppError::DeleteChild(session.clone()));
        }
        let summary = self.summary(summarized)?;
        self.read_cleanup(session);
        Ok(SessionSummary {
            cleanup: self.cleanup(session),
            ..summary
        })
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
                .is_ok_and(|found| at_work(found.status, self.background_of(member)))
        };
        let below_is_active = family.iter().skip(1).any(active);
        Ok(SessionSummary {
            id: session.clone(),
            title: summarized.title.clone(),
            project: self.project(&summarized.project)?.name.clone(),
            orchestrator: summarized.role.is_orchestrator(),
            children: family.len() - 1,
            status: if summarized.status == Status::Done && below_is_active {
                Status::Running
            } else {
                summarized.status
            },
            background: self.background_of(session),
            stops: family.iter().any(active),
            cleanup: Vec::new(),
        })
    }

    /// What this process knows of its sessions beyond what is stored, for the view-models,
    /// with the sessions that [wait for their orchestrator](Self::waits_for_orchestrator) in
    /// `delegated`.
    fn live_view<'a>(&'a self, delegated: &'a HashSet<SessionId>) -> Live<'a> {
        Live {
            state: &self.live,
            delegated,
        }
    }

    /// How many background tasks `session`'s agent runs.
    fn background_of(&self, session: &SessionId) -> usize {
        self.live(session).map_or(0, |live| live.background)
    }

    /// Records that `session`'s agent runs `count` background tasks.
    fn set_background(&mut self, session: &SessionId, count: usize) {
        if count == 0 {
            if let Some(live) = self.live_mut(session) {
                live.background = 0;
            }
        } else {
            self.live_entry(session).background = count;
        }
    }

    /// `session` and every session below it, `session` first.
    fn family(&self, session: &SessionId) -> Vec<SessionId> {
        let mut family = vec![session.clone()];
        for child in &self.sessions {
            if child.role.parent() == Some(session) {
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
    /// [`Idle`](Status::Idle) and is titled "untitled". It has no backend id until the
    /// backend answers with [`BackendEvent::SessionCreated`]. A project session in a project
    /// with at least two repositories besides its root is an orchestrator.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownProject`] or [`AppError::UnknownRepository`] when `project` or
    /// the scope's repository does not exist, [`AppError::Backend`] when the backend cannot queue
    /// the command (no session is created then).
    pub fn new_session(&mut self, project: ProjectId, scope: Scope) -> Result<SessionId, AppError> {
        let choices = SessionChoices {
            backend: self.default.clone(),
            model: None,
            effort: None,
            auto_allow: false,
            title_source: TitleSource::Derived,
        };
        let id = self.create_session(
            project,
            scope,
            None,
            NEW_SESSION_TITLE.into(),
            choices,
            Workspace::CheckedOut,
        )?;
        self.current = Some(id.clone());
        Ok(id)
    }

    /// Creates a session in `project` with `parent`, `title` and `choices`, running on the
    /// configured backend `choices.backend` and working in `workspace` (in a worktree's
    /// folder for a worktree), and asks the backend to start it, as
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
        choices: SessionChoices,
        workspace: Workspace,
    ) -> Result<SessionId, AppError> {
        let cwd = match &workspace {
            Workspace::Worktree { path, .. } => path.clone(),
            Workspace::CheckedOut | Workspace::NewBranch { .. } => {
                self.working_directory(&project, &scope)?
            }
        };
        let role = if is_orchestrator(self.project(&project)?, &scope) {
            Role::Orchestrator
        } else {
            parent.map_or(Role::Plain, |parent| Role::Child { parent })
        };
        let session = Session {
            id: SessionId::new(),
            project,
            scope,
            cwd,
            title,
            backend_session: None,
            backend: choices.backend,
            model: choices.model,
            effort: choices.effort,
            status: Status::Idle,
            role,
            auto_allow: choices.auto_allow,
            instructions_injected: false,
            created_at: self.clock.now(),
            archived: None,
            workspace,
            child_defaults: None,
            first_prompt: String::new(),
            skill: None,
            title_source: choices.title_source,
            last_activity: 0,
            children: crate::Fold::Folded,
            workflow: None,
            question: None,
            wait_reported: None,
            seen_branch: None,
            resolved_cwd: None,
            history: Some(HistoryMarks::default()),
        };
        let request = self.next_request();
        self.backend.new_session(
            request,
            session.id.clone(),
            &session.backend,
            &session.cwd,
            session.tool_role(),
        )?;
        let id = session.id.clone();
        let project = session.project.clone();
        self.requests
            .pending
            .insert(request, (id.clone(), Request::Create));
        self.live_entry(&id).transcript = TranscriptState::Loaded(Transcript::default());
        self.sessions.push(session);
        self.save_sessions(&project);
        Ok(id)
    }

    /// Makes `session` the current session. The first time it is selected its transcript is
    /// loaded in the background: its [view](Self::session_view) shows it
    /// [loading](SessionViewModel::loading) until the load arrives through
    /// [`apply_store_event`](Self::apply_store_event). Selecting it again meanwhile queues no
    /// second load. A load that fails is reported then, and the view says
    /// [the history could not be read](SessionViewModel::load_failed) until the session is
    /// selected again, which loads it again.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists; the current session is
    /// unchanged then.
    pub fn select_session(&mut self, session: SessionId) -> Result<(), AppError> {
        self.start_load(&session)?;
        self.current = Some(session);
        Ok(())
    }

    /// Builds the session's transcript from `updates`, its stored records. The history
    /// is the record; the view stored with the session is only a cache of it, so a view that
    /// has applied fewer workflow events than the history holds is rebuilt from it and saved.
    /// A permission request it holds that Partitur is not waiting on was left unanswered by a
    /// quit, so it is settled as cancelled in the transcript and the store: the agent that
    /// asked is gone, and an open request would keep the session in a turn it is not in. Its
    /// undecided proposals join the ones the user has not decided, so a proposal made before
    /// a restart still holds its orchestrator.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists; the transcript stays
    /// unloaded then.
    fn load_transcript(
        &mut self,
        id: &SessionId,
        updates: &[TranscriptRecord],
    ) -> Result<(), AppError> {
        let project = self.session(id)?.project.clone();
        self.heal_marks(&project, id, updates);
        let mut transcript = Transcript::from_updates(updates);
        self.refresh_workflow(id, &transcript, updates)?;
        let stale: Vec<PermissionId> = transcript
            .unanswered_permissions()
            .filter(|permission| {
                !self
                    .permissions
                    .waiting
                    .contains_key(&(id.clone(), *permission))
            })
            .collect();
        for permission in stale {
            let lapsed = TranscriptRecord::PermissionAnswered {
                id: permission,
                option: None,
            };
            self.append(&project, id.clone(), lapsed.clone());
            transcript.apply(&lapsed);
        }
        let live = self.live_entry(id);
        for (proposal, children) in transcript.pending_proposals() {
            live.proposals.insert(proposal.clone(), children.to_vec());
        }
        live.transcript = TranscriptState::Loaded(transcript);
        Ok(())
    }

    /// Sets the session's [history marks](HistoryMarks) to those of `updates`, its whole
    /// history as just loaded, and saves the session list when they differed: marks the
    /// history outgrew, after a crash between a record and the save of the list, a record
    /// the store failed to write or a version that kept no marks, are healed so.
    fn heal_marks(&mut self, project: &ProjectId, id: &SessionId, updates: &[TranscriptRecord]) {
        let marks = Some(HistoryMarks::of(updates));
        let stale = self
            .sessions
            .iter_mut()
            .find(|session| &session.id == id)
            .filter(|session| session.history != marks);
        if let Some(session) = stale {
            session.history = marks;
            self.save_sessions(project);
        }
    }

    /// The selected session, if any.
    pub fn current_session(&self) -> Option<SessionId> {
        self.current.clone()
    }

    /// The sessions that are not archived, of every project, in the order the app holds
    /// them. Cheaper than [`tree`](Self::tree) for telling whether sessions came or went.
    pub fn live_sessions(&self) -> Vec<SessionId> {
        self.sessions
            .iter()
            .filter(|session| session.archived.is_none())
            .map(|session| session.id.clone())
            .collect()
    }

    /// Builds the navigation tree: open projects in the order they were opened, each with
    /// its sessions newest first (children nested under their parent) and its repositories
    /// as discovered. Sessions created within the same second count the later one as newer.
    /// A session's context usage is known once its transcript is loaded: after it was
    /// created or selected in this run. A session that
    /// [waits for its orchestrator](Self::waits_for_orchestrator) is marked
    /// [`delegated`](crate::SessionNode::delegated). The folders being
    /// [opened](Self::open_project) follow as [`opening`](TreeModel::opening).
    pub fn tree(&self) -> TreeModel {
        let delegated = self.delegated();
        let mut tree = TreeModel::build(&self.projects, &self.sessions, self.live_view(&delegated));
        for project in &mut tree.projects {
            project.cleaning = self.git_work.cleaning(&project.id);
        }
        tree.opening = self.opening_nodes();
        tree
    }

    /// Sends a prompt to the current session.
    ///
    /// Blank text does nothing. The first prompt is kept whole,
    /// with the skill it starts with, and leaves the title alone. The prompt that goes to the
    /// backend carries the session's instruction chain as long as the session
    /// has not received it, and an orchestrator's also carries its preamble between the
    /// instructions and the text; each instruction file that cannot be read is reported in a
    /// notice. The transcript records the text as typed.
    ///
    /// A session whose backend session is not attached yet (still being created, never
    /// created, or restored from the store) queues the prompt and asks the backend to create
    /// or load it; the prompt goes out once that succeeds. Either way the session becomes
    /// [`Running`](Status::Running).
    ///
    /// A prompt to a session that waits at a gate closes the gate, as decided by the user's
    /// prompt rather than approved; the agent reads the prompt for what to change. The gate
    /// closes only once the backend has taken the command, so a prompt that cannot be queued
    /// leaves it open.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoCurrentSession`] when no session is selected, [`AppError::Archived`]
    /// when the session is archived, [`AppError::UnknownBackend`] when the session's backend is no
    /// longer configured, [`AppError::TurnInProgress`] when the session is
    /// [`Running`](Status::Running) or a permission request of its turn waits for an answer (an
    /// orchestrator that waits for you only for the decision on its proposal takes the prompt) and
    /// [`AppError::Backend`] when the backend cannot queue the command (nothing changes in these
    /// cases, and an open gate stays open).
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use partitur_core::testing::{
    ///     Call, MemoryFiles, MemoryGit, MemoryStore, RecordingBackend, open_project, single_claude,
    /// };
    /// use partitur_core::{App, BackendEvent, BackendSessionId, GlobalSettings, PromptContent, Scope};
    ///
    /// let backend = RecordingBackend::new();
    /// let (backends, default) = single_claude();
    /// let (store, git) = (MemoryStore::new(), Box::new(MemoryGit::new()));
    /// let files = MemoryFiles::new();
    /// files.add_folder("/work/shop");
    /// let global = GlobalSettings::default();
    /// let mut app = App::load(
    ///     Box::new(backend.clone()),
    ///     Box::new(store.clone()),
    ///     Box::new(store),
    ///     git,
    ///     Box::new(files.clone()),
    ///     backends,
    ///     default,
    ///     global,
    ///     Box::new(partitur_core::testing::FixedClock::default()),
    /// )?;
    /// let project = open_project(&mut app, &files, "/work/shop".as_ref())?;
    /// let session = app.new_session(project, Scope::Project)?;
    /// let calls = backend.calls();
    /// let (request, _, _) = calls.first().and_then(Call::as_new_session).ok_or("no create")?;
    ///
    /// // The backend has not created the session yet, so the prompt waits for it.
    /// app.submit_prompt("Fix the build")?;
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
    /// assert_eq!(view.breadcrumb.last().map(String::as_str), Some("untitled"));
    /// assert!(view.running());
    /// # Ok(())
    /// # }
    /// ```
    pub fn submit_prompt(&mut self, text: &str) -> Result<(), AppError> {
        let id = self.current.clone().ok_or(AppError::NoCurrentSession)?;
        self.check_promptable(&id)?;
        if text.trim().is_empty() {
            return Ok(());
        }
        self.start_user_turn(&id, text, Vec::new(), &[])
    }

    /// Submits a prompt to `session`, whichever session is selected, with attachments that
    /// were already read and copied in the background.
    ///
    /// It does what [`submit_prompt`](Self::submit_prompt) does, with the contents of
    /// `prepared` sent after the text in order. The transcript records the text and then, when
    /// `prepared` is not empty, what was sent. A prompt with neither text nor attachments is
    /// ignored.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`submit_prompt`](Self::submit_prompt), including
    /// [`AppError::TurnInProgress`] while the session is in a turn.
    pub fn submit_prepared(
        &mut self,
        session: &SessionId,
        text: &str,
        prepared: Vec<Prepared>,
    ) -> Result<(), AppError> {
        self.check_promptable(session)?;
        if text.trim().is_empty() && prepared.is_empty() {
            return Ok(());
        }
        let (contents, sent): (Vec<_>, Vec<_>) = prepared
            .into_iter()
            .map(|prepared| (prepared.content, prepared.sent))
            .unzip();
        self.start_user_turn(session, text, contents, &sent)
    }

    /// Starts a turn the user began with `text` and `attachments`, records what was sent as
    /// attachments when `sent` is not empty and closes the session's open question with the
    /// text.
    fn start_user_turn(
        &mut self,
        id: &SessionId,
        text: &str,
        attachments: Vec<PromptContent>,
        sent: &[SentAttachment],
    ) -> Result<(), AppError> {
        let mut records = vec![TranscriptRecord::UserText(text.to_owned())];
        if !sent.is_empty() {
            records.push(TranscriptRecord::UserAttachments(sent.to_vec()));
        }
        self.start_turn(
            id,
            &records,
            text.to_owned(),
            attachments,
            TurnOrigin::User,
            Some(Closing {
                by: GateCloser::Prompt,
                decider: Decider::User,
            }),
        )?;
        self.close_question(
            id,
            QuestionOutcome::Answered(text.to_owned()),
            Decider::User,
        )
    }

    /// The folder where the attachments sent in `session` are kept, which nothing has created
    /// until the first one is written.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists and
    /// [`AppError::Store`] when the store cannot name the folder.
    pub fn attachment_dir(&self, session: &SessionId) -> Result<PathBuf, AppError> {
        let project = self.session(session)?.project.clone();
        Ok(self.store.attachment_dir(project, session.clone())?)
    }

    /// Whether the session takes a user prompt now.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::Archived`] when it is archived, [`AppError::UnknownBackend`] when its
    /// backend is no longer configured and [`AppError::TurnInProgress`] when it is in a turn.
    fn check_promptable(&self, id: &SessionId) -> Result<(), AppError> {
        let session = self.session(id)?;
        if session.archived.is_some() {
            return Err(AppError::Archived(id.clone()));
        }
        if !self.is_configured(&session.backend) {
            return Err(AppError::UnknownBackend(session.backend.clone()));
        }
        if self.in_turn(id)? {
            return Err(AppError::TurnInProgress);
        }
        Ok(())
    }

    /// Starts a turn of the session with `text` and `attachments`, started by `origin`, and
    /// records `records` in its transcript, in order and before any notice the turn adds.
    ///
    /// With `closing` set, the session's open gate is closed by the user that way, recorded
    /// before `records` and only after the backend has taken the command, so a command that
    /// cannot be queued leaves the gate open.
    ///
    /// The prompt goes to the attached backend session (with the instructions and an
    /// orchestrator's preamble as [`submit_prompt`](Self::submit_prompt) describes), or waits
    /// for the backend session to be created or loaded. Each instruction file that cannot be
    /// read is reported in a notice, and the session becomes [`Running`](Status::Running).
    ///
    /// The session's first turn, whatever starts it, records `text` whole as its first prompt,
    /// together with the skill it starts with. A turn is the first while the session has no
    /// first prompt and its transcript holds nothing but notices. A session whose transcript
    /// already has a conversation (one saved before first prompts were kept, or whose first
    /// prompt was attachments only) keeps an empty first prompt rather than take a prompt from
    /// the middle of it.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists and [`AppError::Backend`]
    /// when the backend cannot queue the command (nothing changes then).
    fn start_turn(
        &mut self,
        id: &SessionId,
        records: &[TranscriptRecord],
        text: String,
        attachments: Vec<PromptContent>,
        origin: TurnOrigin,
        closing: Option<Closing>,
    ) -> Result<(), AppError> {
        let first = self.session(id)?.first_prompt.is_empty() && !self.has_history(id)?;
        let first_prompt = first.then(|| text.clone());
        let prompt = QueuedPrompt { text, attachments };
        let skipped = if self.is_attached(id)? && !self.loading(id) {
            self.send_prompt(id, prompt)?
        } else {
            if !self.attached(id) {
                self.request_backend_session(id.clone())?;
            }
            self.live_entry(id).queued.push(prompt);
            Vec::new()
        };
        if let Some(Closing { by, decider }) = closing {
            self.close_gate(id, by, decider)?;
        }
        self.live_entry(id).turn_origin = Some(origin);
        for record in records {
            self.apply_update(id, record)?;
        }
        self.report_skipped(id, skipped)?;
        self.set_status(id.clone(), Status::Running)?;
        let Some(text) = first_prompt else {
            return Ok(());
        };
        self.change_session(id.clone(), |session| {
            session.skill = split_skill(&text).0;
            session.first_prompt = text;
        })
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
    /// Returns [`AppError::NoCurrentSession`] when no session is selected and [`AppError::Backend`]
    /// when the backend cannot queue the command.
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
    /// Returns [`AppError::UnknownSession`] when no such session exists (nothing changes then) and
    /// [`AppError::Backend`] when the backend cannot queue a command. A failure ends the call
    /// there: the children stopped before it stay stopped, the others keep running.
    pub fn stop_children(&mut self, session: &SessionId) -> Result<(), AppError> {
        self.session(session)?;
        let children: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|child| child.role.parent() == Some(session))
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
        if self
            .live_mut(&id)
            .is_some_and(|live| !std::mem::take(&mut live.queued).is_empty())
        {
            self.set_status(id.clone(), Status::Idle)?;
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
    /// Returns [`AppError::Backend`] when the backend cannot queue the answer (the request keeps
    /// waiting).
    pub fn answer_permission(
        &mut self,
        session: SessionId,
        permission: PermissionId,
        option: Option<PermissionOptionId>,
    ) -> Result<(), AppError> {
        let waiting = (session, permission);
        if !self.permissions.waiting.contains_key(&waiting) {
            return Ok(());
        }
        let (session, permission) = waiting;
        self.backend
            .respond_permission(session.clone(), permission, option.clone())?;
        self.permissions
            .waiting
            .remove(&(session.clone(), permission));
        self.drop_asked(&session, permission);
        let answered = TranscriptRecord::PermissionAnswered {
            id: permission,
            option,
        };
        self.apply_update(&session, &answered)?;
        if self.permissions.any_of(&session) {
            return Ok(());
        }
        self.set_status(session, Status::Running)
    }

    /// The permission requests of the direct children of `orchestrator` that wait for the
    /// user, oldest first by the order they arrived in, each with the child's repository, the
    /// request's title and its options. Answering one with
    /// [`answer_permission`](Self::answer_permission) for the child, here or in the child's
    /// own session, takes it off the list; so does the end of the child's turn.
    pub fn child_permissions(&self, orchestrator: &SessionId) -> Vec<ChildPermission> {
        self.permissions
            .asked
            .iter()
            .filter_map(|(child, permission, title)| {
                let session = self.session(child).ok()?;
                if session.role.parent() != Some(orchestrator) {
                    return None;
                }
                let project = self.project(&session.project).ok()?;
                let options = self
                    .permissions
                    .waiting
                    .get(&(child.clone(), *permission))?;
                Some(ChildPermission {
                    child: child.clone(),
                    repository: scope_label(project, &session.scope),
                    permission: *permission,
                    title: title.clone(),
                    options: options.clone(),
                })
            })
            .collect()
    }

    /// Grants the session's permission requests without asking while `enabled`.
    ///
    /// Enabling it also answers the requests already waiting, by the same rule as new ones:
    /// the first "allow once" option, else the first "allow always" one; a request offering
    /// neither keeps waiting.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists and [`AppError::Backend`]
    /// when an answer cannot be queued.
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

    /// The command a link clicked in the session's transcript is opened with.
    ///
    /// The project's settings are the ones read when the project was loaded or opened, so an
    /// edit applies after a restart. A repository session takes `link_opener` from its
    /// repository, then the project, then the global settings; a project session skips the
    /// repository.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists and
    /// [`AppError::Store`] when the project's settings cannot be read or are invalid.
    pub fn link_opener(&self, session: &SessionId) -> Result<LinkOpener, AppError> {
        let session = self.session(session)?;
        let settings = self.project_settings(&session.project)?;
        let repository = match &session.scope {
            Scope::Project => None,
            Scope::Repository(repository) => Some(repository),
        };
        Ok(settings.link_opener(&self.global.layer, repository))
    }

    /// The settings read for `project` when it was loaded or opened.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Unavailable`] with the reason the file could not be read or was
    /// invalid, and the same when the project's settings were never read.
    pub(crate) fn project_settings(
        &self,
        project: &ProjectId,
    ) -> Result<&ProjectSettings, StoreError> {
        self.settings
            .get(project)
            .ok_or_else(|| StoreError::Unavailable(format!("{project} has no settings read")))?
            .as_ref()
            .map_err(|reason| StoreError::Unavailable(reason.clone()))
    }

    /// Switches the session to the model `value`.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists, [`AppError::NoSuchConfig`]
    /// when its backend offers no model option and [`AppError::Backend`] when the backend cannot
    /// queue the command.
    pub fn set_model(&mut self, session: SessionId, value: ConfigValueId) -> Result<(), AppError> {
        self.set_config(&session, ConfigCategory::Model, &value)?;
        self.change_session(session, |changed| changed.model = Some(value))
    }

    /// Switches the session to the reasoning effort `value`.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists, [`AppError::NoSuchConfig`]
    /// when its backend offers no effort option and [`AppError::Backend`] when the backend cannot
    /// queue the command.
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
    /// [`AppError::UnknownSession`] when no such session exists, [`AppError::TurnInProgress`] when
    /// the session is [`Running`](Status::Running) or a permission request of its turn waits for an
    /// answer (an orchestrator that waits for you only for the decision on its proposal can
    /// switch), [`AppError::Store`] when the session's transcript cannot be read (nothing changes
    /// in these cases), and [`AppError::Backend`] when the backend cannot queue the create. The
    /// switch has been made and saved by then; the next prompt asks for the backend session again.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use partitur_core::testing::{
    ///     Call, MemoryFiles, MemoryGit, MemoryStore, RecordingBackend, open_project,
    /// };
    /// use partitur_core::{App, BackendId, BackendInfo, GlobalSettings, Scope};
    ///
    /// let (claude, work) = (BackendId::try_from("claude")?, BackendId::try_from("work")?);
    /// let backends = [&claude, &work]
    ///     .map(|id| BackendInfo {
    ///         id: id.clone(),
    ///         name: id.to_string(),
    ///         command: "claude-agent-acp".into(),
    ///     })
    ///     .to_vec();
    /// let backend = RecordingBackend::new();
    /// let (store, git) = (MemoryStore::new(), Box::new(MemoryGit::new()));
    /// let files = MemoryFiles::new();
    /// files.add_folder("/work/shop");
    /// let global = GlobalSettings::default();
    /// let mut app = App::load(
    ///     Box::new(backend.clone()),
    ///     Box::new(store.clone()),
    ///     Box::new(store),
    ///     git,
    ///     Box::new(files.clone()),
    ///     backends,
    ///     claude,
    ///     global,
    ///     Box::new(partitur_core::testing::FixedClock::default()),
    /// )?;
    /// let project = open_project(&mut app, &files, "/work/shop".as_ref())?;
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
        if let Some(live) = self.live_mut(&session) {
            live.attached = false;
            live.background = 0;
            live.queued.clear();
            live.config.clear();
        }
        self.requests.forget(&session);
        self.lapse_permissions(&session)?;
        if notify {
            let notice =
                format!("switched to {backend}; the new agent has no memory of earlier turns");
            self.apply_update(&session, &TranscriptRecord::Notice(notice))?;
        }
        self.change_session(session.clone(), |switched| {
            switched.backend = backend;
            switched.backend_session = None;
            switched.instructions_injected = false;
            switched.model = None;
            switched.effort = None;
        })?;
        self.set_status(session.clone(), Status::Idle)?;
        self.request_backend_session(session)
    }

    /// The view of the selected session, or `None` when no session is selected or its
    /// transcript was never asked for. It carries the
    /// [permission requests of the session's children](Self::child_permissions) that wait for
    /// the user. While the transcript loads, the view has no entries and is
    /// [loading](SessionViewModel::loading); after a load that failed it has none and says
    /// so with [`load_failed`](SessionViewModel::load_failed).
    pub fn session_view(&self) -> Option<SessionViewModel<'_>> {
        let session = self.session(self.current.as_ref()?).ok()?;
        let project = self.project(&session.project).ok()?;
        let transcript = self.shown_transcript(&session.id)?;
        let delegated = self.delegated();
        let mut view = SessionViewModel::build(
            project,
            &self.sessions,
            self.live_view(&delegated),
            session,
            transcript,
            self.config_of(&session.id),
            &self.backends,
        );
        view.loading = self.loading(&session.id);
        view.load_failed = self.load_failed_for(&session.id);
        view.approval = self.approval_view(&session.id);
        view.child_permissions = self.child_permissions(&session.id);
        view.attachment_dir = self
            .store
            .attachment_dir(session.project.clone(), session.id.clone())
            .ok();
        Some(view)
    }

    /// The environment of the selected session, or `None` when no session is selected or its
    /// transcript was never asked for. While the transcript loads, or when its load failed,
    /// what the view takes from it is empty.
    ///
    /// The instruction files are read from disk on every call, so the view shows them as
    /// they are now.
    pub fn env_view(&self) -> Option<EnvViewModel> {
        let session = self.session(self.current.as_ref()?).ok()?;
        let project = self.project(&session.project).ok()?;
        let transcript = self.shown_transcript(&session.id)?;
        let mut model = EnvViewModel::build(
            project,
            &self.sessions,
            session,
            session_instructions(project, session),
            transcript,
            self.config_of(&session.id),
            &self.backends,
        );
        if let Some(card) = model.git.as_mut() {
            card.live = self.env_live(&session.id);
            card.outside = outside_paths(transcript, &self.boundary(session));
        }
        Some(model)
    }

    /// The transcript the views of `session` show: the loaded one, an empty one while it
    /// loads or after its load failed, and `None` when it was never asked for.
    fn shown_transcript(&self, session: &SessionId) -> Option<&Transcript> {
        match &self.live(session)?.transcript {
            TranscriptState::Loaded(transcript) => Some(transcript),
            TranscriptState::Loading { .. } | TranscriptState::Failed => Some(&NOT_LOADED),
            TranscriptState::Unloaded => None,
        }
    }

    /// Where `session` works, for the Git card's files outside its folder: its folder as it is
    /// recorded and as a reading of git resolved it; the temporary folder (both forms)
    /// and the session's attachment folder ignored; and the home folder. Nothing is resolved
    /// here, so the view never waits on the file system.
    fn boundary(&self, session: &Session) -> Boundary {
        let mut ignored = self.temp_dirs.clone();
        ignored.extend(
            self.store
                .attachment_dir(session.project.clone(), session.id.clone())
                .ok(),
        );
        let inside = std::iter::once(session.cwd.clone())
            .chain(session.resolved_cwd.clone())
            .collect();
        Boundary {
            inside,
            ignored,
            home: self.global.home.clone(),
        }
    }

    /// Applies a git outcome to the surface that asked for it. The answer to the reading of
    /// the session being prepared fills its branch box. The fresh reading Create asks for,
    /// and then the outcome of its git work, carry the create on, as
    /// [`create_from_draft`](Self::create_from_draft) describes. A create whose sheet
    /// was cancelled is undone once it succeeded; a failed one made nothing. An answer nothing
    /// waits for any more is ignored. Afterwards the branch box of the session being prepared
    /// is checked again, since the repositories busy may have changed. The reading of a
    /// delete sheet fills its cleanup lines, and the fresh reading of a deleted session's
    /// repository runs the removal checked there, as [`delete`](Self::delete) describes.
    /// A split card's failed reading of a repository the request no longer keeps busy is
    /// forgotten, so the card's next check reads it again.
    ///
    /// # Errors
    ///
    /// Returns what went wrong after the asking surface is gone or cannot show it:
    /// [`AppError::Undo`] when an undo cannot be queued or fails, the
    /// [`AppError::Backend`] or [`AppError::Store`] errors of starting a created session,
    /// [`AppError::Cleanup`] when a removal cannot be queued or fails, and
    /// [`AppError::CleanupKept`] when a checked worktree or branch was kept.
    pub fn apply_git_event(&mut self, event: GitEvent) -> Result<(), AppError> {
        let finished = self.git_work.finish(event.request()).map(|(work, freed)| {
            self.forget_failed_reads(&freed);
            work
        });
        let applied = match (finished, event) {
            (Some(Work::DraftInspect), GitEvent::Answered { request, answers }) => {
                self.draft_inspected(request, answers);
                Ok(())
            }
            (Some(Work::DraftCheck { planned }), GitEvent::Answered { request, answers }) => {
                self.draft_checked(request, &planned, answers);
                Ok(())
            }
            (Some(Work::DraftCreate { planned }), GitEvent::Ran { result, .. }) => {
                self.draft_created(&planned, result)
            }
            (
                Some(Work::Abandoned {
                    repository,
                    workspace,
                    before,
                }),
                GitEvent::Ran { result: Ok(()), .. },
            ) => self.undo(&repository, &workspace, &before),
            (
                Some(Work::SplitInspect {
                    proposal,
                    repositories,
                }),
                GitEvent::Answered { request, answers },
            ) => {
                self.split_inspected(request, &proposal, repositories, answers);
                Ok(())
            }
            (Some(Work::ApprovalCheck(pressed)), GitEvent::Answered { answers, .. }) => {
                self.approval_checked(pressed, answers);
                Ok(())
            }
            (Some(Work::Approve { approving, fetches }), GitEvent::Ran { done, result, .. }) => {
                self.approval_ran(approving, fetches, done, result)
            }
            (
                Some(Work::Undo { repositories }),
                GitEvent::Ran {
                    done,
                    result: Err(error),
                    ..
                },
            ) => Err(AppError::Undo {
                repository: repositories.get(done).cloned().unwrap_or_default(),
                error,
            }),
            (Some(Work::EnvCheck { session, folder }), GitEvent::Answered { request, answers }) => {
                self.env_git_read(&session, &folder, request, answers);
                Ok(())
            }
            (Some(Work::CleanupCheck { session }), GitEvent::Answered { request, answers }) => {
                self.cleanup_read(&session, request, answers);
                Ok(())
            }
            (Some(Work::RemoveCheck(removal)), GitEvent::Answered { answers, .. }) => {
                self.removal_checked(&removal, answers)
            }
            (
                Some(Work::Remove { repository, .. }),
                GitEvent::Ran {
                    result: Err(error), ..
                },
            ) => Err(AppError::Cleanup { repository, error }),
            _ => Ok(()),
        };
        self.refresh_draft();
        applied
    }

    /// Queues the undo of `workspace`, which a create made in `repository` while its main
    /// checkout had `before` checked out; the repository stays busy until it has run.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Undo`] when the git port cannot queue it.
    fn undo(
        &mut self,
        repository: &Repository,
        workspace: &Workspace,
        before: &Head,
    ) -> Result<(), AppError> {
        let steps = undo_steps(&repository.path, workspace, before);
        let repositories = vec![repository.name.clone(); steps.len()];
        self.queue_undo(steps, repositories)
    }

    /// Queues the undo `steps`, each restoring the repository named at its place in
    /// `repositories`; they stay busy until it has run.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Undo`], naming every repository the steps restore, when the git
    /// port cannot queue it.
    fn queue_undo(
        &mut self,
        steps: Vec<GitStep>,
        repositories: Vec<String>,
    ) -> Result<(), AppError> {
        let mut named: Vec<&str> = Vec::new();
        for name in &repositories {
            if !named.contains(&name.as_str()) {
                named.push(name);
            }
        }
        let named = named.join(", ");
        match self
            .git_work
            .run(&*self.git, Work::Undo { repositories }, steps)
        {
            Ok(_) => Ok(()),
            Err(error) => Err(AppError::Undo {
                repository: named,
                error,
            }),
        }
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
    /// [`BackendEvent::Update`] goes into the store and then the session's transcript, except
    /// [`AgentUpdate::RunningBackground`], which only sets the session's running background
    /// tasks in memory (they end with the agent's process, so they are never stored). In an
    /// orchestrator session a permission request for one of Partitur's own tools, or to read
    /// only files inside the worktrees of its children (links resolved), is allowed at once
    /// when it offers an allowing option, and one that would edit, delete, move or
    /// execute is rejected at once (cancelled when it offers no rejection) with a notice
    /// saying so. Any other permission request
    /// makes the session [`WaitingForYou`](Status::WaitingForYou), or
    /// with auto-allow is answered at once with the first "allow once" option (else the first
    /// "allow always" one); the end of a turn makes it [`Done`](Status::Done), a failure
    /// [`Failed`](Status::Failed); both settle its waiting permission requests. New config
    /// options replace the session's and set its model and effort to their current values.
    /// A config change the agent rejected, when it is the latest pick for its option, puts that
    /// option back to the value the agent reported last and records it, leaving other changes
    /// on their way alone, so prompts held for a model change go out on the model the agent
    /// still runs; the transcript shows every rejection as a notice.
    /// A turn the agent starts without a prompt makes the session
    /// [`Running`](Status::Running) and is recorded in the transcript; the same signal while
    /// the session is already in a turn is ignored, since that turn is already running.
    ///
    /// An update or a close for a session that is not attached comes from a connection the
    /// session no longer uses (it switched backends, or its connection already closed) and
    /// is ignored.
    ///
    /// Any event for a session deleted, or a warm session discarded, in this process is
    /// ignored. An event for the warm session of the [session being
    /// prepared](Self::prepare_session) updates the draft instead: the answer to its create
    /// makes it ready, and sets the wanted model on it where the agent lists that model but
    /// runs another, or makes it failed; a close fails it, new config options replace its
    /// options once it is ready, a rejected change to the latest model picked puts back the
    /// model the agent runs, and any other update and the rejection are held until the session
    /// is created.
    ///
    /// An orchestrator whose turn ends while one of its proposals is undecided waits for you
    /// instead of being done.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when the event names a session that does not exist and
    /// [`AppError::Backend`] when a follow-up command cannot be queued.
    pub fn apply_event(&mut self, event: BackendEvent) -> Result<(), AppError> {
        if self.deleted.contains(event_session(&event)) {
            return Ok(());
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
                if update == AgentUpdate::TurnStarted && self.in_turn(&session)? {
                    return Ok(());
                }
                if let AgentUpdate::Title(title) = update {
                    return self.apply_agent_title(&session, title);
                }
                if let AgentUpdate::RunningBackground(count) = update {
                    self.set_background(&session, count);
                    return Ok(());
                }
                let record = TranscriptRecord::from(update);
                self.apply_update(&session, &record)?;
                self.react(&session, record)
            }
            BackendEvent::SessionClosed { session, error } => {
                if !self.is_attached(&session)? {
                    return Ok(());
                }
                self.close_session(&session, &error.to_string())
            }
            BackendEvent::RequestFailed {
                request,
                session,
                error,
            } => match self.settle(request, &session)? {
                Some(Request::Load) => self.fall_back_to_new_session(&session),
                Some(Request::Create) => self.fail_session(&session, error.to_string()),
                None => Ok(()),
            },
        }
    }

    /// Handles a session's tool call and answers it through the call's reply.
    ///
    /// Any session may report its workflow; the report is recorded and answered with the text
    /// that tells the agent whether to stop. The tools below are an orchestrator's.
    /// `propose_children` records the proposal and `send_to_child` prompts the child (or queues the
    /// prompt while the child is busy) and records the prompt in the orchestrator's transcript;
    /// `list_children` lists the orchestrator's children, `cancel_child` stops a child's current
    /// turn and `get_child_result` returns a child's last final message with its status; a call
    /// that is not valid is answered with the reason and changes nothing. A call from a session
    /// deleted in this process is answered with `the session was deleted`, one from the warm
    /// session of the [session being prepared](Self::prepare_session) with `unknown session`. A
    /// call whose handling fails is answered with the failure's text before the failure is
    /// returned. Whether the caller still waits is checked when the call is taken up; a call nobody
    /// waits for any more changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Backend`] when a prompt cannot be queued.
    pub fn call_tool(&mut self, call: ToolCall) -> Result<(), AppError> {
        self.tool_call_received(call)
    }

    /// Folds an open project in the tree, or opens a folded one, and saves the project list.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownProject`] when no such project exists.
    pub fn toggle_project(&mut self, project: ProjectId) -> Result<(), AppError> {
        let Some(found) = self.projects.iter_mut().find(|open| open.id == project) else {
            return Err(AppError::UnknownProject(project));
        };
        found.fold = found.fold.toggled();
        self.save_projects();
        Ok(())
    }

    /// Shows the children below a session in the tree, or hides them, and saves the session
    /// list. The session's last activity is left alone.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists.
    pub fn toggle_children(&mut self, session: SessionId) -> Result<(), AppError> {
        self.change_session(session, |toggled| {
            toggled.children = toggled.children.toggled();
        })
    }

    /// Gives the session the title the user typed, ignoring surrounding whitespace, so an
    /// agent's title never replaces it. A blank title changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists.
    pub fn rename_session(&mut self, session: SessionId, title: &str) -> Result<(), AppError> {
        self.session(&session)?;
        let title = title.trim();
        if title.is_empty() {
            return Ok(());
        }
        self.change_session(session, |renamed| {
            title.clone_into(&mut renamed.title);
            renamed.title_source = TitleSource::Given;
        })
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
        self.save_sessions(&project);
        Ok(())
    }

    /// Sets the session's status and stamps its last activity with the current time.
    fn set_status(&mut self, id: SessionId, status: Status) -> Result<(), AppError> {
        let now = self.clock.now();
        self.change_session(id, |session| {
            session.status = status;
            session.last_activity = now;
        })
    }

    /// Names the session `title` when its title is [derived](TitleSource::Derived). The title
    /// is not part of the transcript, so it is only saved with the session list.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when the session does not exist.
    fn apply_agent_title(&mut self, session: &SessionId, title: String) -> Result<(), AppError> {
        if self.session(session)?.title_source != TitleSource::Derived {
            return Ok(());
        }
        self.change_session(session.clone(), |named| named.title = title)
    }

    /// Whether the session is attached: its backend session was created or loaded by this
    /// process, and it has neither closed nor switched backends since.
    fn is_attached(&self, id: &SessionId) -> Result<bool, AppError> {
        self.session(id)?;
        Ok(self.attached(id))
    }

    /// Whether the session's transcript has at least one entry, by the session's
    /// [history marks](HistoryMarks::has_entries); no history is read.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] for an unknown session and [`AppError::Store`]
    /// when its history could not be read when it was loaded.
    fn has_entries(&self, id: &SessionId) -> Result<bool, AppError> {
        Ok(self.marks(id)?.has_entries)
    }

    /// Whether the session's transcript holds anything besides notices: a prompt, an answer
    /// or another sign of a conversation, by the session's
    /// [history marks](HistoryMarks::has_history); no history is read.
    ///
    /// # Errors
    ///
    /// As [`has_entries`](Self::has_entries).
    fn has_history(&self, id: &SessionId) -> Result<bool, AppError> {
        Ok(self.marks(id)?.has_history)
    }

    /// The marks of the session's history.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] for an unknown session and [`AppError::Store`]
    /// when the store could not read its history to fill them in when it loaded the session.
    fn marks(&self, id: &SessionId) -> Result<&HistoryMarks, AppError> {
        self.session(id)?.history.as_ref().ok_or_else(|| {
            AppError::Store(StoreError::Unavailable(
                "the history of this session could not be read when Partitur started".into(),
            ))
        })
    }

    /// Whether `backend` is one of the configured backends.
    fn is_configured(&self, backend: &BackendId) -> bool {
        self.backends.iter().any(|info| &info.id == backend)
    }

    fn config_of(&self, session: &SessionId) -> &[ConfigOption] {
        self.live(session)
            .map_or(&[], |live| live.config.as_slice())
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
        if self.requests.any_for(&id) {
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
                session.tool_role(),
            )?;
            Request::Load
        } else {
            self.backend.new_session(
                request,
                id.clone(),
                &session.backend,
                &session.cwd,
                session.tool_role(),
            )?;
            Request::Create
        };
        self.requests.pending.insert(request, (id, kind));
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
        Ok(self.requests.pending.remove(&request).map(|(_, kind)| kind))
    }

    /// Completes a successful create or load: the session is attached, its config options
    /// are stored, the model and effort it recorded are re-applied as
    /// [`reapply_choices`](Self::reapply_choices) describes, and its waiting prompts are sent,
    /// unless a model change went out: then they wait for the options of the new model.
    fn attach(&mut self, session: &SessionId, config: Vec<ConfigOption>) -> Result<(), AppError> {
        self.store_config(session, config);
        self.live_entry(session).attached = true;
        self.reapply_choices(session, false)?;
        if !self.effort_pending(session) {
            self.send_queued(session)?;
        }
        self.change_session(session.clone(), |_| {})
    }

    /// Sends the prompts waiting for the session, in order, unless its transcript is loading:
    /// they wait for the load then.
    fn send_queued(&mut self, session: &SessionId) -> Result<(), AppError> {
        if self.loading(session) {
            return Ok(());
        }
        let queued = self
            .live_mut(session)
            .map(|live| std::mem::take(&mut live.queued))
            .unwrap_or_default();
        for prompt in queued {
            let skipped = self.send_prompt(session, prompt)?;
            self.report_skipped(session, skipped)?;
        }
        Ok(())
    }

    /// Sends the session's recorded model and effort to the backend where its config options
    /// list the value among their choices and report another current value. A recorded value
    /// the option does not list is replaced by the option's current value in memory, without
    /// telling the backend; the caller saves. A category the backend does not offer is left
    /// alone.
    ///
    /// The agent lists only the efforts of its current model. So while a model change is on
    /// its way, because `switching` says one is or because this sends one, the options still
    /// list the old model's efforts: the recorded effort is kept as it is and applied when
    /// the options for the new model arrive, as [`replace_config`](Self::replace_config)
    /// describes.
    fn reapply_choices(&mut self, session: &SessionId, switching: bool) -> Result<(), AppError> {
        let recorded = self.session(session)?;
        let (model, effort) = (recorded.model.clone(), recorded.effort.clone());
        let (model, sent) = self.reapplied(session, ConfigCategory::Model, model)?;
        let effort = if switching || sent {
            self.live_entry(session).effort_pending = true;
            effort
        } else {
            if let Some(live) = self.live_mut(session) {
                live.effort_pending = false;
            }
            self.reapplied(session, ConfigCategory::Effort, effort)?.0
        };
        let reapplied = self.session_mut(session)?;
        reapplied.model = model;
        reapplied.effort = effort;
        Ok(())
    }

    /// Re-applies the recorded `value` of `category` as
    /// [`reapply_choices`](Self::reapply_choices) describes and returns the value the session
    /// records from now on, and whether it was sent to the backend.
    fn reapplied(
        &mut self,
        session: &SessionId,
        category: ConfigCategory,
        value: Option<ConfigValueId>,
    ) -> Result<(Option<ConfigValueId>, bool), AppError> {
        let Some(value) = value else {
            return Ok((None, false));
        };
        let Some(option) = self
            .config_of(session)
            .iter()
            .find(|option| option.category == category)
        else {
            return Ok((Some(value), false));
        };
        if !option.choices.iter().any(|choice| choice.id == value) {
            return Ok((Some(option.current.clone()), false));
        }
        let sent = option.current != value;
        if sent {
            self.set_config(session, category, &value)?;
        }
        Ok((Some(value), sent))
    }

    /// The session's permission requests waiting for an answer, oldest first, with their
    /// options.
    fn waiting_permissions(
        &self,
        session: &SessionId,
    ) -> Vec<(PermissionId, Vec<PermissionOption>)> {
        let mut waiting: Vec<_> = self
            .permissions
            .waiting
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
        self.apply_update(id, &TranscriptRecord::Notice(RESUMED_NOTICE.into()))
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
        if let Some(live) = self.live_mut(id) {
            live.attached = false;
            live.background = 0;
            live.queued.clear();
        }
        self.lapse_permissions(id)?;
        let notice = format!("connection to the agent closed: {error}");
        self.apply_update(id, &TranscriptRecord::Notice(notice.clone()))?;
        if !in_turn {
            return Ok(());
        }
        self.set_status(id.clone(), Status::Failed)?;
        self.settle_turn(id, ChildOutcome::Failed(notice), true)
    }

    /// Fails a session whose backend session could not be created: its waiting prompts are
    /// dropped and a notice says why. A child that was in a turn reports `error` to its
    /// parent as the failure; prompts Partitur deferred for the session stay queued, since a
    /// session that cannot be created would only fail them again.
    fn fail_session(&mut self, id: &SessionId, error: String) -> Result<(), AppError> {
        let in_turn = self.in_turn(id)?;
        if let Some(live) = self.live_mut(id) {
            live.queued.clear();
            live.background = 0;
        }
        self.set_status(id.clone(), Status::Failed)?;
        self.apply_update(id, &TranscriptRecord::Notice(error.clone()))?;
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
            Status::WaitingForYou => self.has_waiting_permission(id),
            Status::Idle | Status::Interrupted | Status::Done | Status::Failed => false,
        })
    }

    /// Whether a permission request of the session waits for an answer.
    fn has_waiting_permission(&self, id: &SessionId) -> bool {
        self.permissions.any_of(id)
    }

    /// Sends `prompt` to the session's attached backend session, with the instruction chain
    /// (and an orchestrator's preamble, or a child's title note and the note that its
    /// orchestrator decides for it, after it) before the text when
    /// the session has not received it yet. Returns the instruction files that had to be skipped; the caller
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
            let text = if session.role.is_orchestrator() {
                format!("{}\n\n{}", preamble(project, &session.cwd), prompt.text)
            } else if session.role.parent().is_some() {
                format!(
                    "{}\n\n{}\n\n{}",
                    titles::child_title_note(&session.title),
                    crate::orchestrator::CHILD_ROLE,
                    prompt.text
                )
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
            self.apply_update(session, &TranscriptRecord::Notice(notice))?;
        }
        Ok(())
    }

    /// Updates the session's status and config after `update` was applied.
    fn react(&mut self, session: &SessionId, update: TranscriptRecord) -> Result<(), AppError> {
        match update {
            TranscriptRecord::PermissionRequested {
                id,
                title,
                kind,
                options,
                partitur_tool,
                locations,
                ..
            } => self.permission_requested(
                session,
                id,
                PermissionCall {
                    title,
                    kind,
                    partitur_tool,
                    locations,
                },
                options,
            ),
            TranscriptRecord::TurnStarted => {
                self.live_entry(session).turn_origin = Some(TurnOrigin::Agent);
                self.set_status(session.clone(), Status::Running)
            }
            TranscriptRecord::TurnEnded(reason) => {
                self.end_turn(session, Status::Done, ChildOutcome::Ended(reason))
            }
            TranscriptRecord::Failed(error) => {
                self.end_turn(session, Status::Failed, ChildOutcome::Failed(error))
            }
            TranscriptRecord::ConfigOptions(options) => self.replace_config(session, options),
            TranscriptRecord::ConfigRejected { option, value, .. } => {
                self.config_rejected(session, &option, &value)
            }
            _ => Ok(()),
        }
    }

    /// Records the request `permission` for `call`, offering `options`, and answers it as
    /// [`apply_event`](Self::apply_event) describes, or leaves it waiting for the user.
    fn permission_requested(
        &mut self,
        session: &SessionId,
        permission: PermissionId,
        call: PermissionCall,
        options: Vec<PermissionOption>,
    ) -> Result<(), AppError> {
        let PermissionCall {
            title,
            kind,
            partitur_tool,
            locations,
        } = call;
        let title = title.as_str();
        if partitur_tool && let Some(option) = allowing_option(&options) {
            self.permissions
                .waiting
                .insert((session.clone(), permission), options);
            return self.answer_permission(session.clone(), permission, Some(option));
        }
        if self.session(session)?.role.is_orchestrator() {
            let own_worktrees = reads_only_in(kind, &locations, &self.child_worktrees(session));
            if own_worktrees && let Some(option) = allowing_option(&options) {
                self.permissions
                    .waiting
                    .insert((session.clone(), permission), options);
                return self.answer_permission(session.clone(), permission, Some(option));
            }
            let workflow_document =
                writes_only_in_workflow_folders(kind, &locations, &self.session(session)?.cwd);
            if !partitur_tool
                && !workflow_document
                && let Some(word) = restrained(kind)
            {
                let rejection = rejecting_option(&options);
                self.permissions
                    .waiting
                    .insert((session.clone(), permission), options);
                self.answer_permission(session.clone(), permission, rejection)?;
                return self.apply_update(
                    session,
                    &TranscriptRecord::Notice(denial_notice(word, title)),
                );
            }
        }
        let automatic = if self.session(session)?.auto_allow {
            allowing_option(&options)
        } else {
            None
        };
        self.permissions
            .waiting
            .insert((session.clone(), permission), options);
        if let Some(option) = automatic {
            return self.answer_permission(session.clone(), permission, Some(option));
        }
        self.permissions
            .asked
            .push((session.clone(), permission, title.to_owned()));
        self.set_status(session.clone(), Status::WaitingForYou)
    }

    /// Forgets that the request `permission` of `session` waits for the user.
    fn drop_asked(&mut self, session: &SessionId, permission: PermissionId) {
        self.permissions
            .asked
            .retain(|(asked, id, _)| (asked, *id) != (session, permission));
    }

    /// The worktrees of the session's children.
    fn child_worktrees(&self, session: &SessionId) -> Vec<&Path> {
        self.sessions
            .iter()
            .filter(|child| child.role.parent() == Some(session))
            .filter_map(|child| match &child.workspace {
                Workspace::Worktree { path, .. } => Some(path.as_path()),
                Workspace::CheckedOut | Workspace::NewBranch { .. } => None,
            })
            .collect()
    }

    /// Ends the session's turn with `status`: its unanswered permission requests lapse, a
    /// child reports `outcome` to its parent, and the next prompt Partitur deferred for the
    /// session goes out. A cancelled turn reports too but sends nothing: a stopped session
    /// stays quiet until the user prompts it, and its queue moves on when a later turn ends
    /// otherwise. A session that would be [`Done`](Status::Done) with an undecided proposal
    /// is [`WaitingForYou`](Status::WaitingForYou) instead: it waits for the decision, but its
    /// turn is over, so it takes a user prompt and its next deferred prompt goes out as after
    /// any other turn; the turn that starts ends in [`WaitingForYou`](Status::WaitingForYou)
    /// again while the proposal is undecided. A session that would be
    /// [`Done`](Status::Done) with a gate or a question open waits for you the same way, until
    /// the user approves the gate, answers the question or prompts it. A turn the agent
    /// started on its own ends the same way and reports to the parent as started by the agent.
    fn end_turn(
        &mut self,
        session: &SessionId,
        status: Status,
        outcome: ChildOutcome,
    ) -> Result<(), AppError> {
        let waits = self.awaits_decision(session)
            || self.gate_open(session).is_some()
            || self.question_open(session);
        let status = if status == Status::Done && waits {
            Status::WaitingForYou
        } else {
            status
        };
        self.lapse_permissions(session)?;
        self.set_status(session.clone(), status)?;
        let drain = outcome != ChildOutcome::Ended(StopReason::Cancelled);
        self.settle_turn(session, outcome, drain)
    }

    /// Settles the session's waiting permission requests as cancelled, in the transcript and
    /// the store, without telling the backend: the agent can no longer receive the answers.
    fn lapse_permissions(&mut self, session: &SessionId) -> Result<(), AppError> {
        for (permission, _) in self.waiting_permissions(session) {
            self.permissions
                .waiting
                .remove(&(session.clone(), permission));
            self.drop_asked(session, permission);
            let lapsed = TranscriptRecord::PermissionAnswered {
                id: permission,
                option: None,
            };
            self.apply_update(session, &lapsed)?;
        }
        Ok(())
    }

    /// Takes `options` as the session's config options, and their current model and effort
    /// as the ones the session records. The first options after a model change that
    /// [`reapply_choices`](Self::reapply_choices) left pending list the new model's efforts:
    /// the recorded effort is re-applied against them instead. The prompts waiting for the
    /// session, which an attached session holds only until then, are sent.
    fn replace_config(
        &mut self,
        session: &SessionId,
        options: Vec<ConfigOption>,
    ) -> Result<(), AppError> {
        let model = current_value(&options, &ConfigCategory::Model);
        let effort = current_value(&options, &ConfigCategory::Effort);
        self.store_config(session, options);
        let effort = self.settled_effort(session, effort)?;
        self.change_session(session.clone(), |changed| {
            changed.model = model;
            changed.effort = effort;
        })?;
        self.send_queued(session)
    }

    /// The effort the session records now that the agent settled its model, whose options
    /// list `current` as the effort in effect: after a model change that
    /// [`reapply_choices`](Self::reapply_choices) left pending, the recorded effort is
    /// re-applied against the options and kept where they list it; otherwise, and when the
    /// model offers no effort, `current`. The pending mark is cleared.
    fn settled_effort(
        &mut self,
        session: &SessionId,
        current: Option<ConfigValueId>,
    ) -> Result<Option<ConfigValueId>, AppError> {
        let pending = self
            .live_mut(session)
            .is_some_and(|live| std::mem::take(&mut live.effort_pending));
        if pending && current.is_some() {
            let recorded = self.session(session)?.effort.clone();
            return Ok(self
                .reapplied(session, ConfigCategory::Effort, recorded)?
                .0
                .or(current));
        }
        Ok(current)
    }

    /// Undoes the change of the session's option `option` to `value`, which the agent
    /// refused, when `value` is still the latest one picked for the option; a rejection of an
    /// earlier pick leaves the later one on its way. The option goes back to the value the
    /// agent reported last, and only that option: other changes on their way are still
    /// unanswered. The session records the model and effort the options then hold. A model
    /// the agent refused also ends the wait of a pending model change: the effort is settled
    /// as [`settled_effort`](Self::settled_effort) describes. The prompts waiting for the
    /// session go out unless a model change is still pending.
    fn config_rejected(
        &mut self,
        session: &SessionId,
        option: &ConfigId,
        value: &ConfigValueId,
    ) -> Result<(), AppError> {
        let Some(LiveSession {
            config,
            unconfirmed,
            ..
        }) = self.live.get_mut(session)
        else {
            return Ok(());
        };
        let Some((rejected, reported)) = config
            .iter_mut()
            .find(|candidate| candidate.id == *option)
            .filter(|rejected| rejected.current == *value)
            .and_then(|rejected| Some((rejected, unconfirmed.remove(option)?)))
        else {
            return Ok(());
        };
        rejected.current = reported;
        let model_refused = rejected.category == ConfigCategory::Model;
        let options = self.config_of(session);
        let model = current_value(options, &ConfigCategory::Model);
        let mut effort = current_value(options, &ConfigCategory::Effort);
        if model_refused {
            effort = self.settled_effort(session, effort)?;
        }
        self.change_session(session.clone(), |changed| {
            changed.model = model;
            changed.effort = effort;
        })?;
        if self.effort_pending(session) {
            return Ok(());
        }
        self.send_queued(session)
    }

    /// Stores `options`, a full report from the agent, as the session's config options. The
    /// values kept for its unanswered changes are dropped: the report says what the agent
    /// runs now.
    fn store_config(&mut self, session: &SessionId, options: Vec<ConfigOption>) {
        let live = self.live_entry(session);
        live.unconfirmed.clear();
        live.config = options;
    }

    /// Asks the backend to set the session's option of `category` to `value` and records the
    /// value as the option's current one; the value the agent reported last is kept until it
    /// answers.
    fn set_config(
        &mut self,
        session: &SessionId,
        category: ConfigCategory,
        value: &ConfigValueId,
    ) -> Result<(), AppError> {
        self.session(session)?;
        let Some(LiveSession {
            config,
            unconfirmed,
            ..
        }) = self.live.get_mut(session)
        else {
            return Err(AppError::NoSuchConfig(category));
        };
        let Some(option) = config.iter_mut().find(|option| option.category == category) else {
            return Err(AppError::NoSuchConfig(category));
        };
        self.backend
            .set_config(session.clone(), &option.id, value)?;
        unconfirmed
            .entry(option.id.clone())
            .or_insert_with(|| option.current.clone());
        option.current = value.clone();
        Ok(())
    }

    /// Records `record` in the history of `session` as Partitur itself does, for the tests of
    /// other crates that set up a transcript holding what no backend reports, such as the
    /// user's prompts or an orchestrator's proposal.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] for a session the app does not know.
    #[cfg(any(test, feature = "testing"))]
    pub fn record(
        &mut self,
        session: &SessionId,
        record: &TranscriptRecord,
    ) -> Result<(), AppError> {
        self.apply_update(session, record)
    }

    /// Queues `record` for the history of `session` and applies it to the session's
    /// transcript when that is loaded, without waiting for the store; an unloaded transcript
    /// picks it up from the store when it is loaded, and one that is loading keeps it aside
    /// for after the load, which does not see it. A record the store then fails to write
    /// stays in the transcript, and the failure arrives as a [`StoreEvent::Failed`].
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists.
    fn apply_update(
        &mut self,
        session: &SessionId,
        record: &TranscriptRecord,
    ) -> Result<(), AppError> {
        let project = self.session(session)?.project.clone();
        self.append(&project, session.clone(), record.clone());
        match self.live_mut(session).map(|live| &mut live.transcript) {
            Some(TranscriptState::Loaded(transcript)) => transcript.apply(record),
            Some(TranscriptState::Loading { later, .. }) => later.push(record.clone()),
            Some(TranscriptState::Unloaded | TranscriptState::Failed) | None => {}
        }
        Ok(())
    }

    /// Queues `record` for the history of `session`, a session of `project`, and applies it
    /// to the session's [history marks](HistoryMarks). When they change, the session list
    /// is saved after the record, so the stored marks say what the history holds.
    fn append(&mut self, project: &ProjectId, session: SessionId, record: TranscriptRecord) {
        let marked = self
            .sessions
            .iter_mut()
            .find(|marked| marked.id == session)
            .and_then(|marked| marked.history.as_mut())
            .is_some_and(|marks| marks.apply(&record));
        self.queue.send(StoreCommand::Append {
            project: project.clone(),
            session,
            record,
        });
        if marked {
            self.save_sessions(project);
        }
    }

    /// Queues the save of the project list.
    fn save_projects(&self) {
        self.queue
            .send(StoreCommand::SaveProjects(self.projects.clone()));
    }

    /// Queues the save of the sessions of `project`.
    fn save_sessions(&self, project: &ProjectId) {
        let sessions: Vec<Session> = self
            .sessions
            .iter()
            .filter(|session| &session.project == project)
            .cloned()
            .collect();
        self.queue.send(StoreCommand::SaveSessions {
            project: project.clone(),
            sessions,
        });
    }
}

/// Whether the stored `session` was in a turn when Partitur closed, by the rule
/// [`App::in_turn`] applies to a live one: a running session is, and a waiting one only while
/// a permission request of its turn is unanswered, which its
/// [history marks](HistoryMarks::has_open_permission) tell. A session that waits only for a
/// decision on a proposal had its turn end already. Only a waiting session whose marks the
/// store could not fill in has its history read, as a start-up may.
///
/// # Errors
///
/// Returns [`StoreError`] when a waiting session without marks has updates that cannot be
/// loaded.
fn left_in_turn(store: &dyn Store, session: &Session) -> Result<bool, StoreError> {
    Ok(match session.status {
        Status::Running => true,
        Status::WaitingForYou => match &session.history {
            Some(marks) => marks.has_open_permission(),
            None => Transcript::from_updates(
                &store.load_records(session.project.clone(), session.id.clone())?,
            )
            .unanswered_permissions()
            .next()
            .is_some(),
        },
        Status::Idle | Status::Interrupted | Status::Done | Status::Failed => false,
    })
}

/// The current value of the first option of `category` in `options`, `None` when no option
/// has the category.
fn current_value(options: &[ConfigOption], category: &ConfigCategory) -> Option<ConfigValueId> {
    options
        .iter()
        .find(|option| &option.category == category)
        .map(|option| option.current.clone())
}

/// The session `event` names.
fn event_session(event: &BackendEvent) -> &SessionId {
    match event {
        BackendEvent::SessionCreated { session, .. }
        | BackendEvent::SessionLoaded { session, .. }
        | BackendEvent::Update { session, .. }
        | BackendEvent::SessionClosed { session, .. }
        | BackendEvent::RequestFailed { session, .. } => session,
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

/// The tool call a permission request is about.
struct PermissionCall {
    /// Human-readable summary of the call.
    title: String,
    /// What the call does.
    kind: ToolKind,
    /// Whether the tool belongs to Partitur's own tool server.
    partitur_tool: bool,
    /// The paths the call is about.
    locations: Vec<PathBuf>,
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

/// The media type of an image the agent can be sent inline, judged by the file extension.
pub fn image_mime(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

#[cfg(test)]
mod tests;

/// Reads the settings of `project` for [`App`] to keep: the file, or why it cannot be used.
fn read_settings(store: &dyn Store, project: &ProjectId) -> Result<ProjectSettings, String> {
    store
        .load_settings(project.clone())
        .map_err(|error| error.to_string())
}
