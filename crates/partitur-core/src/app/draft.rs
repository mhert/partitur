//! The session the new-session sheet prepares before it exists.
//!
//! Opening the sheet starts a warm session: a backend session on the seeded backend, in the
//! folder and with the orchestrator flag the real session will have, so the agent reports the
//! models and efforts it offers before anything is created. The newest session of the project
//! seeds the backend, model, effort and auto-allow; a seed whose backend is no longer
//! configured gives only its auto-allow, and a project without sessions starts on the default
//! backend with the agent's own choices and auto-allow off. While the sheet is open the warm
//! session lives outside the session list and the store. The wanted model is set on the warm
//! session as soon as its agent answers, where the agent lists it, and picking a model sets it
//! at once, so the Effort list the agent reports next is the wanted model's. When the agent
//! rejects the change, the model it runs becomes the wanted one again. Effort and
//! auto-allow are only recorded, and picking another backend closes the warm session and warms
//! up a fresh one under a new id.
//!
//! Create adopts the warm session as the real one under the same id: it is saved, selected and,
//! once the agent has answered, attached with its options and the updates it reported so far.
//! The model and effort it records are the wanted ones where the agent lists them, else the
//! agent's current ones; while a model change is still on its way, the wanted effort is kept
//! and checked against the new model's efforts once the agent reports them. Cancel closes the
//! warm session and leaves nothing behind: nothing is saved and every event it still reports
//! is ignored.
//!
//! In Worktree and New branch mode, Create first has git make the branch, in the background,
//! and the sheet waits for it, frozen, with Cancel still offered. Only once git has made it is
//! the session created; a failed step creates nothing and shows why in the sheet. A Worktree
//! session runs in its worktree, so the warm session, which started in the repository, is
//! closed and a fresh session starts in the worktree. Whatever git made for a session that is
//! then not created, because it could not be saved or the sheet was cancelled meanwhile, is
//! undone.
//!
//! The branch box shows how the session will use git: the modes it can work in (Worktree,
//! New branch, Checked-out), the new branch's name, the base it starts from, whether that base
//! is fetched first, where a worktree would go, and why Create cannot be pressed. The newest
//! session of the project seeds the mode and the fetch wish (an orchestrator's from the
//! defaults it keeps for its children); without one the box starts Checked-out with fetching
//! on. The name always starts empty, and the base starts at the branch checked out now. The
//! worktree folder follows the most specific settings layer that sets `worktree_path`.
//!
//! Opening the sheet reads the session's repositories in the background, through the git port,
//! and returns at once. Until git answers, the box shows that it is loading: it lists no
//! bases, Worktree and New branch wait for the answer, and Checked-out can be created already.
//! A name typed or a fetch wish set meanwhile is kept. An answer for a sheet that was closed
//! is ignored.

use std::path::{Path, PathBuf};

use super::git_work::Work;
use super::orchestration::UNKNOWN_SESSION;
use super::workspace::{
    Branching, Context, Planned, RepoState, Target, create_steps, workspace_of,
};
use super::{App, AppError, NEW_SESSION_TITLE, Request, unix_seconds};
use crate::backend::{BackendEvent, ConfigCategory, ConfigOption, Update};
use crate::git::{GitAnswer, GitError, GitQuery};
use crate::ids::{
    BackendId, BackendSessionId, ConfigId, ConfigValueId, GitRequestId, ProjectId, RequestId,
    SessionId, ToolRequestId,
};
use crate::orchestrator::is_orchestrator;
use crate::session::{BranchChoice, BranchMode, Scope, Session, Status, Workspace};
use crate::transcript::Transcript;
use crate::view_model::{DraftChoice, DraftView, Warmup, option_view};

/// The session being prepared: what the sheet has chosen and how far its warm session is.
pub(super) struct Draft {
    /// The warm session's id, which the created session keeps unless it starts afresh in a
    /// worktree.
    id: SessionId,
    /// The request that started the warm session.
    request: RequestId,
    /// What the created session will be.
    settings: Settings,
    /// How far the warm session has started.
    phase: Phase,
    /// The updates the warm session reported, in arrival order, applied on create.
    held: Vec<Update>,
    /// The git request of the Create in flight, `None` while none is.
    creating: Option<GitRequestId>,
    /// Why the last Create failed.
    failure: Option<String>,
}

/// What [`App::create_from_draft`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Created {
    /// The session was created and selected.
    Session(SessionId),
    /// Its git work is queued; the draft waits for it, and the session is created once git
    /// has made the branch.
    Pending,
}

/// What the sheet has chosen for the session, which carries over to a fresh warm-up.
struct Settings {
    project: ProjectId,
    scope: Scope,
    cwd: PathBuf,
    orchestrator: bool,
    backend: BackendId,
    /// The model wanted, kept where the agent lists it.
    model: Option<ConfigValueId>,
    /// The effort wanted, kept where the agent lists it.
    effort: Option<ConfigValueId>,
    auto_allow: bool,
    /// The branch box.
    branch: Branching,
}

/// How far a warm session has started.
enum Phase {
    /// The create is on its way.
    Loading,
    /// The agent answered with its backend id and options.
    Ready {
        backend_session: BackendSessionId,
        config: Vec<ConfigOption>,
        /// The model the agent runs while a model change it has not answered yet is on its
        /// way, so `config` still lists that model's efforts.
        switching: Option<ConfigValueId>,
    },
    /// The agent could not start, for this reason.
    Failed(String),
}

impl App {
    /// Prepares a session in `project` for the new-session sheet and starts its warm
    /// session, as the [module](self) describes. A session being prepared already is
    /// discarded first, as [`discard_draft`](Self::discard_draft) does.
    ///
    /// The warm session works in the project root or the repository's folder, and is an
    /// orchestrator when the session will be one. A warm-up the backend cannot queue does not
    /// fail the call: the draft shows the failure instead.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownProject`] or [`AppError::UnknownRepository`] when `project`
    /// or the scope's repository does not exist; nothing changes then.
    pub fn prepare_session(&mut self, project: ProjectId, scope: Scope) -> Result<(), AppError> {
        let cwd = self.working_directory(&project, &scope)?;
        let found = self.project(&project)?.clone();
        let orchestrator = is_orchestrator(&found, &scope);
        // The open draft is gone even when its close cannot be queued; only the outcome of
        // the new warm-up is reported, in the draft.
        drop(self.discard_draft());
        let target = Target::read(&found, &scope, orchestrator, |queries| {
            match self.git_work.query(&*self.git, Work::DraftInspect, queries) {
                Ok(request) => RepoState::Loading(request),
                Err(error) => RepoState::Failed(error.to_string()),
            }
        });
        let project_settings = self
            .store
            .load_settings(project.clone())
            .map_err(|error| error.to_string());
        let choice = self.seed_branch(&project);
        let mut branch = Branching::new(found, target, choice, project_settings);
        self.refresh(&mut branch);
        let mut settings = Settings {
            project,
            scope,
            cwd,
            orchestrator,
            backend: self.default.clone(),
            model: None,
            effort: None,
            auto_allow: false,
            branch,
        };
        self.seed(&mut settings);
        self.warm_up(settings);
        Ok(())
    }

    /// The session being prepared, or `None` when the sheet is closed.
    pub fn draft(&self) -> Option<DraftView> {
        let draft = self.draft.as_ref()?;
        let settings = &draft.settings;
        let project = self.project(&settings.project).ok()?;
        let repository = match &settings.scope {
            Scope::Project => None,
            Scope::Repository(id) => project
                .repositories
                .iter()
                .find(|repository| &repository.id == id)
                .map(|repository| repository.name.clone()),
        };
        Some(DraftView {
            project: project.name.clone(),
            repository,
            repositories: project.repositories.len(),
            orchestrator: settings.orchestrator,
            backend: settings.backend.clone(),
            backends: self.backends.clone(),
            model: draft.choice(&ConfigCategory::Model, settings.model.as_ref()),
            effort: draft.choice(&ConfigCategory::Effort, settings.effort.as_ref()),
            auto_allow: settings.auto_allow,
            warmup: draft.warmup(),
            branch: settings.branch.view().clone(),
            creating: draft.creating.is_some(),
            failure: draft.failure.clone(),
        })
    }

    /// Picks the branch mode of the session being prepared.
    ///
    /// Every `draft_*` change clears the reason the last Create failed. While the git work of
    /// a Create is in flight, each of them returns [`AppError::Workspace`] (`the session is
    /// being created`) and changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared and
    /// [`AppError::Workspace`] while it is being created or when the session is not offered
    /// `mode`; nothing changes then.
    pub fn draft_mode(&mut self, mode: BranchMode) -> Result<(), AppError> {
        self.change_branch(|branch| branch.set_mode(mode))
    }

    /// Names the new branch of the session being prepared, as typed; surrounding spaces are
    /// ignored when it is checked and used.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared and
    /// [`AppError::Workspace`] while it is being created.
    pub fn draft_branch(&mut self, name: String) -> Result<(), AppError> {
        self.change_branch(|branch| {
            branch.set_branch(name);
            Ok(())
        })
    }

    /// Picks the branch the new branch of the session being prepared starts from.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared and
    /// [`AppError::Workspace`] while it is being created or when `base` is not among the bases
    /// offered, as while git has not answered yet; nothing changes then.
    pub fn draft_base(&mut self, base: String) -> Result<(), AppError> {
        self.change_branch(|branch| branch.set_base(base))
    }

    /// Sets whether the base of the session being prepared is fetched first.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared and
    /// [`AppError::Workspace`] while it is being created or when fetching is turned on for a
    /// loaded base without a remote counterpart; nothing changes then.
    pub fn draft_fetch(&mut self, fetch: bool) -> Result<(), AppError> {
        self.change_branch(|branch| branch.set_fetch(fetch))
    }

    /// Applies the `answers` git gave to `request` to the branch box, when the session being
    /// prepared still waits for them.
    pub(super) fn draft_inspected(
        &mut self,
        request: GitRequestId,
        answers: Vec<Result<GitAnswer, GitError>>,
    ) {
        if let Some(draft) = self.draft.as_mut() {
            draft.settings.branch.answer(request, answers);
        }
    }

    /// Checks the branch box of the session being prepared again, against the sessions, the
    /// global settings and the repositories busy now.
    pub(super) fn refresh_draft(&mut self) {
        if let Some(mut draft) = self.draft.take() {
            self.refresh(&mut draft.settings.branch);
            self.draft = Some(draft);
        }
    }

    /// Moves the session being prepared to the configured backend `backend`: the warm
    /// session is closed and a fresh one, under a new id, starts on `backend`. The wanted
    /// model, effort and auto-allow stay, and the pickers wait for the new agent. Picking the
    /// backend the warm session runs on changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared,
    /// [`AppError::Workspace`] while it is being created, [`AppError::UnknownBackend`] when
    /// `backend` is not configured and [`AppError::Backend`] when the warm session cannot be
    /// closed; nothing changes then.
    pub fn draft_backend(&mut self, backend: BackendId) -> Result<(), AppError> {
        let draft = self.editable_draft()?;
        let (running, warm) = (draft.settings.backend.clone(), draft.id.clone());
        if !self.is_configured(&backend) {
            return Err(AppError::UnknownBackend(backend));
        }
        if running == backend {
            return Ok(());
        }
        self.backend.close_session(warm)?;
        if let Some(old) = self.draft.take() {
            self.deleted.insert(old.id);
            self.warm_up(Settings {
                backend,
                ..old.settings
            });
        }
        Ok(())
    }

    /// Picks the model `value` for the session being prepared and sets it on the warm
    /// session at once, so the agent can report the efforts that model offers.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared,
    /// [`AppError::Workspace`] while it is being created, [`AppError::NoSuchConfig`] when its
    /// agent has not answered yet or offers no model, and [`AppError::Backend`] when the
    /// backend cannot queue the change; nothing changes then.
    pub fn draft_model(&mut self, value: ConfigValueId) -> Result<(), AppError> {
        self.editable_draft()?;
        self.set_draft_model(value)
    }

    /// Picks the reasoning effort `value` for the session being prepared; it is sent when
    /// the session is created.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared and
    /// [`AppError::Workspace`] while it is being created.
    pub fn draft_effort(&mut self, value: ConfigValueId) -> Result<(), AppError> {
        self.editable_draft()?.settings.effort = Some(value);
        Ok(())
    }

    /// Sets whether the session being prepared will grant permission requests without
    /// asking.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared and
    /// [`AppError::Workspace`] while it is being created.
    pub fn draft_auto_allow(&mut self, enabled: bool) -> Result<(), AppError> {
        self.editable_draft()?.settings.auto_allow = enabled;
        Ok(())
    }

    /// Creates the session being prepared and selects it, once git has made its branch.
    ///
    /// The session is titled "New session" and starts [`Idle`](Status::Idle) with the chosen
    /// backend and auto-allow. Except in Worktree mode it adopts the warm session and keeps
    /// its id: once the agent has answered, the session is attached with the agent's backend
    /// id and options, records the wanted model and effort where the agent lists them (else
    /// the agent's current ones), gets the updates the agent reported so far, and the values
    /// that differ from the agent's current ones are sent. Before the agent has answered, the
    /// session records the wanted values as they are, and they are applied when the answer
    /// arrives.
    ///
    /// Create goes in this order:
    ///
    /// 1. The draft must exist, must not be creating already, and its agent must have started.
    /// 2. The branch box is checked again, against the sessions, settings, folders and busy
    ///    repositories as they are now; a reason that blocks Create fails it.
    /// 3. Checked-out mode, and an orchestrator, need no git work: the session is created at
    ///    once, working in the repository as it is checked out, and [`Created::Session`] is
    ///    returned. An orchestrator records the branch settings for its children, with the
    ///    name trimmed.
    /// 4. Worktree and New branch mode read the repository again, and [`Created::Pending`] is
    ///    returned; the draft is creating and frozen from now on, and the repository is busy
    ///    from the press until the git work has run. When the fresh reading
    ///    arrives it replaces the earlier one in the box, and the box is checked against it:
    ///    the new branch must not exist, the base must, and the work must be the same as the
    ///    sheet showed (the same fetch, for one). A reason that blocks Create, or a reading
    ///    that failed, ends the creating state and shows in the box. Otherwise the git work
    ///    is queued, which keeps the repository busy until it has run: the fetch when wanted,
    ///    then the branch or worktree, made from `<remote>/<base>` when fetching, else from
    ///    `<base>`. A run git cannot queue ends the creating state with the reason.
    ///
    /// The head the main checkout returns to on an undo comes from that fresh reading. A
    /// switch made outside Partitur between that reading and the git work is not seen.
    ///
    /// When git reports back through [`apply_git_event`](Self::apply_git_event), a failed step
    /// made nothing: the draft stays and shows why. Otherwise the session records its
    /// workspace and is saved; when its agent failed meanwhile or the save fails, the session
    /// is not created, what git made is undone, and the draft stays and shows why. Otherwise
    /// a New branch session adopts the warm session in the repository, and a Worktree session
    /// starts afresh in its worktree, under a new id: the warm session is closed as
    /// [`discard_draft`](Self::discard_draft) closes it, and every event it still reports is
    /// ignored. The draft is gone and the session selected. A backend or store error of
    /// starting it is returned from `apply_git_event`; the session exists all the same.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared,
    /// [`AppError::Workspace`] while it is being created or with the reason the branch box
    /// blocks Create, [`AppError::DraftFailed`] when its agent could not start,
    /// [`AppError::Git`] when the git port cannot queue the work, [`AppError::Store`] when the
    /// session list cannot be saved (in each of these cases the session is not created and the
    /// draft stays as it was), and afterwards [`AppError::Store`] or [`AppError::Backend`]
    /// when a held update cannot be saved or a choice cannot be sent; the session is created
    /// and selected then all the same.
    pub fn create_from_draft(&mut self) -> Result<Created, AppError> {
        let draft = self.editable_draft()?;
        if matches!(draft.phase, Phase::Failed(_)) {
            return Err(AppError::DraftFailed);
        }
        let Some(planned) = self.planned()? else {
            return self.create_at_once().map(Created::Session);
        };
        let read = vec![GitQuery::Inspect(planned.repository.path.clone())];
        let held = vec![planned.repository.path.clone()];
        let request =
            self.git_work
                .query_holding(&*self.git, Work::DraftCheck { planned }, read, held)?;
        if let Some(draft) = self.draft.as_mut() {
            draft.creating = Some(request);
        }
        Ok(Created::Pending)
    }

    /// Discards the session being prepared: the draft is gone, every event its warm
    /// session still reports is ignored, and the warm session is closed. While Create reads
    /// the repository again, nothing has been queued that changes it, so nothing is undone.
    /// While its git work is in flight, the sheet still closes at once: what that work makes
    /// is undone once it has run, and the repository stays busy until then; work that failed
    /// made nothing to undo. An undo that fails is reported by
    /// [`apply_git_event`](Self::apply_git_event).
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared and
    /// [`AppError::Backend`] when the close cannot be queued; the draft is gone either way.
    pub fn discard_draft(&mut self) -> Result<(), AppError> {
        let draft = self.draft.take().ok_or(AppError::NoDraft)?;
        if let Some(request) = draft.creating {
            self.git_work.abandon(request);
        }
        self.deleted.insert(draft.id.clone());
        self.backend.close_session(draft.id)?;
        Ok(())
    }

    /// Whether `session` is the warm session of the session being prepared.
    pub(super) fn is_draft(&self, session: &SessionId) -> bool {
        self.draft
            .as_ref()
            .is_some_and(|draft| &draft.id == session)
    }

    /// Applies `event` of the warm session, as [`apply_event`](Self::apply_event) describes.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Backend`] when the answer to a tool call or the wanted model
    /// cannot be queued.
    pub(super) fn draft_event(&mut self, event: BackendEvent) -> Result<(), AppError> {
        match self.draft.as_mut().and_then(|draft| draft.apply(event)) {
            Some(Reaction::Refuse(call)) => self.refuse_tool(call, UNKNOWN_SESSION),
            Some(Reaction::SetModel(model)) => self.set_draft_model(model),
            None => Ok(()),
        }
    }

    /// Checks the Create of the session being prepared against the fresh reading `answers`
    /// to `request`, and queues its git work when nothing blocks it, as
    /// [`create_from_draft`](Self::create_from_draft) describes; `pressed` is the work the
    /// sheet showed. A reading the draft no longer waits for is ignored.
    pub(super) fn draft_checked(
        &mut self,
        request: GitRequestId,
        pressed: &Planned,
        answers: Vec<Result<GitAnswer, GitError>>,
    ) {
        let Some(mut draft) = self.draft.take_if(|draft| draft.creating == Some(request)) else {
            return;
        };
        draft.creating = None;
        let read = draft.settings.branch.reread(answers);
        self.refresh(&mut draft.settings.branch);
        let fresh = read.map(|()| draft.settings.branch.planned());
        match fresh {
            Err(error) => draft.failure = Some(error),
            Ok(Ok(Some(fresh))) if fresh.same_work(pressed) => {
                let steps = create_steps(&fresh);
                let work = Work::DraftCreate { planned: fresh };
                match self.git_work.run(&*self.git, work, steps) {
                    Ok(run) => draft.creating = Some(run),
                    Err(error) => draft.failure = Some(error.to_string()),
                }
            }
            Ok(Ok(_)) => {
                draft.failure = Some(format!(
                    "{} changed meanwhile; check the branch settings and create again",
                    pressed.repository.name
                ));
            }
            // The box shows the blocker.
            Ok(Err(_)) => {}
        }
        self.draft = Some(draft);
    }

    /// Finishes the Create of the session being prepared, whose git work for `planned` ended
    /// with `result`, as [`create_from_draft`](Self::create_from_draft) describes.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Undo`] when the undo cannot be queued, and the
    /// [`AppError::Backend`] or [`AppError::Store`] errors of starting the created session.
    pub(super) fn draft_created(
        &mut self,
        planned: &Planned,
        result: Result<(), GitError>,
    ) -> Result<(), AppError> {
        // Cancelling turns this work into abandoned work, so a draft is waiting for it.
        self.draft
            .take()
            .map_or(Ok(()), |draft| self.finish_create(draft, planned, result))
    }

    /// Creates the session `draft` prepared once git ran the steps of `planned` with
    /// `result`, or puts the draft back with the reason it failed.
    ///
    /// # Errors
    ///
    /// As [`draft_created`](Self::draft_created).
    fn finish_create(
        &mut self,
        mut draft: Draft,
        planned: &Planned,
        result: Result<(), GitError>,
    ) -> Result<(), AppError> {
        draft.creating = None;
        if let Err(error) = result {
            draft.failure = Some(error.to_string());
            self.draft = Some(draft);
            return Ok(());
        }
        let workspace = workspace_of(planned);
        let fresh = planned.mode == BranchMode::Worktree;
        let saved = draft.session(workspace.clone()).and_then(|mut session| {
            if fresh {
                session.id = SessionId::new();
                session.backend_session = None;
            }
            self.save_session(session)
        });
        match saved {
            Ok(id) if fresh => self.start_fresh(draft, id),
            Ok(id) => {
                self.current = Some(id);
                self.adopt(draft)
            }
            Err(error) => {
                draft.failure = Some(error.to_string());
                self.draft = Some(draft);
                self.undo(&planned.repository, &workspace, &planned.before)
            }
        }
    }

    /// Starts the created session `id` afresh in its folder and selects it, closing the warm
    /// session of `draft` as [`discard_draft`](Self::discard_draft) does.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Backend`] when the warm session cannot be closed or the fresh one
    /// cannot be requested; the session exists all the same.
    fn start_fresh(&mut self, draft: Draft, id: SessionId) -> Result<(), AppError> {
        self.deleted.insert(draft.id.clone());
        let closed = self.backend.close_session(draft.id);
        self.transcripts.insert(id.clone(), Transcript::default());
        self.current = Some(id.clone());
        let requested = self.request_backend_session(id);
        closed.map_err(AppError::from).and(requested)
    }

    /// Creates the session being prepared, which needs no git work, as
    /// [`create_from_draft`](Self::create_from_draft) describes.
    ///
    /// # Errors
    ///
    /// As [`create_from_draft`](Self::create_from_draft).
    fn create_at_once(&mut self) -> Result<SessionId, AppError> {
        let draft = self.draft.as_ref().ok_or(AppError::NoDraft)?;
        let session = draft.session(Workspace::CheckedOut)?;
        let id = self.save_session(session)?;
        let adopted = self.draft.take().map_or(Ok(()), |draft| self.adopt(draft));
        self.current = Some(id.clone());
        adopted.map(|()| id)
    }

    /// Adds `session` to the session list and saves it, returning its id.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Store`] when the list cannot be saved; the session is not added
    /// then.
    fn save_session(&mut self, session: Session) -> Result<SessionId, AppError> {
        let (id, project) = (session.id.clone(), session.project.clone());
        self.sessions.push(session);
        if let Err(error) = self.save_sessions(&project) {
            self.sessions.pop();
            return Err(error);
        }
        Ok(id)
    }

    /// The git work Create does for the session being prepared, checked again now.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared and
    /// [`AppError::Workspace`] with the reason Create is blocked.
    fn planned(&mut self) -> Result<Option<Planned>, AppError> {
        let mut draft = self.draft.take().ok_or(AppError::NoDraft)?;
        self.refresh(&mut draft.settings.branch);
        let planned = draft.settings.branch.planned();
        self.draft = Some(draft);
        planned.map_err(AppError::Workspace)
    }

    /// The session being prepared, when it may change: it exists and its Create is not in
    /// flight. The reason the last Create failed is cleared.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared and
    /// [`AppError::Workspace`] while it is being created.
    fn editable_draft(&mut self) -> Result<&mut Draft, AppError> {
        let draft = self.draft.as_mut().ok_or(AppError::NoDraft)?;
        if draft.creating.is_some() {
            return Err(AppError::Workspace("the session is being created".into()));
        }
        draft.failure = None;
        Ok(draft)
    }

    /// Sets the model `value` on the warm session, as [`draft_model`](Self::draft_model)
    /// describes, also while the session is being created.
    ///
    /// # Errors
    ///
    /// As [`draft_model`](Self::draft_model), without [`AppError::Workspace`].
    fn set_draft_model(&mut self, value: ConfigValueId) -> Result<(), AppError> {
        let draft = self.draft.as_mut().ok_or(AppError::NoDraft)?;
        let missing = AppError::NoSuchConfig(ConfigCategory::Model);
        let Phase::Ready {
            config, switching, ..
        } = &mut draft.phase
        else {
            return Err(missing);
        };
        let option = config
            .iter_mut()
            .find(|option| option.category == ConfigCategory::Model)
            .ok_or(missing)?;
        self.backend
            .set_config(draft.id.clone(), &option.id, &value)?;
        switching.get_or_insert_with(|| option.current.clone());
        option.current = value.clone();
        draft.settings.model = Some(value);
        Ok(())
    }

    /// The newest session of `project`. Of sessions created in the same second, the later one
    /// counts as newer.
    fn newest(&self, project: &ProjectId) -> Option<&Session> {
        self.sessions
            .iter()
            .filter(|session| session.project == *project)
            .max_by_key(|session| session.created_at)
    }

    /// The branch mode and fetch wish the newest session of `project` seeds, as the
    /// [module](self) describes.
    fn seed_branch(&self, project: &ProjectId) -> BranchChoice {
        let (mode, fetch) = match self.newest(project) {
            Some(Session {
                child_defaults: Some(defaults),
                ..
            }) => (defaults.mode, defaults.fetch),
            Some(newest) => (
                newest.workspace.mode(),
                newest.workspace.fetched().unwrap_or(true),
            ),
            None => (BranchMode::CheckedOut, true),
        };
        BranchChoice {
            mode,
            fetch,
            ..BranchChoice::default()
        }
    }

    /// Recomputes the view of `branch` against the sessions, the global settings and the
    /// repositories busy now.
    fn refresh(&self, branch: &mut Branching) {
        let busy = |repository: &Path| self.git_work.is_busy(repository);
        branch.refresh(&Context {
            sessions: &self.sessions,
            global: &self.global,
            busy: &busy,
        });
    }

    /// Applies `change` to the branch box of the session being prepared and recomputes it.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared and
    /// [`AppError::Workspace`] while it is being created or with the reason `change` refused.
    fn change_branch(
        &mut self,
        change: impl FnOnce(&mut Branching) -> Result<(), String>,
    ) -> Result<(), AppError> {
        self.editable_draft()?;
        let mut draft = self.draft.take().ok_or(AppError::NoDraft)?;
        let changed = change(&mut draft.settings.branch);
        self.refresh(&mut draft.settings.branch);
        self.draft = Some(draft);
        changed.map_err(AppError::Workspace)
    }

    /// Takes the backend, wanted model and effort, and auto-allow from the newest session of
    /// the project into `settings`, as the [module](self) describes.
    fn seed(&self, settings: &mut Settings) {
        let Some(newest) = self.newest(&settings.project) else {
            return;
        };
        settings.auto_allow = newest.auto_allow;
        if self.is_configured(&newest.backend) {
            settings.backend = newest.backend.clone();
            settings.model = newest.model.clone();
            settings.effort = newest.effort.clone();
        }
    }

    /// Starts a warm session under a new id for `settings` and makes it the draft. A
    /// create the backend cannot queue fails the draft.
    fn warm_up(&mut self, settings: Settings) {
        let id = SessionId::new();
        let request = self.next_request();
        let queued = self.backend.new_session(
            request,
            id.clone(),
            &settings.backend,
            &settings.cwd,
            settings.orchestrator,
        );
        let phase = match queued {
            Ok(()) => Phase::Loading,
            Err(error) => Phase::Failed(error.to_string()),
        };
        self.draft = Some(Draft {
            id,
            request,
            settings,
            phase,
            held: Vec::new(),
            creating: None,
            failure: None,
        });
    }

    /// Takes over the created session's warm session, as
    /// [`create_from_draft`](Self::create_from_draft) describes.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Store`] when a held update cannot be saved and
    /// [`AppError::Backend`] when a choice cannot be sent.
    fn adopt(&mut self, draft: Draft) -> Result<(), AppError> {
        let id = draft.id;
        self.transcripts.insert(id.clone(), Transcript::default());
        match draft.phase {
            Phase::Ready {
                config, switching, ..
            } => {
                let model = config
                    .iter()
                    .find(|option| option.category == ConfigCategory::Model)
                    .map(|option| (id.clone(), option.id.clone()));
                self.store_config(&id, config);
                if let Some((model, running)) = model.zip(switching.clone()) {
                    self.unconfirmed.insert(model, running);
                }
                self.attached.insert(id.clone());
                for update in &draft.held {
                    self.apply_update(&id, update)?;
                }
                self.reapply_choices(&id, switching.is_some())
            }
            Phase::Loading | Phase::Failed(_) => {
                self.pending.insert(draft.request, (id, Request::Create));
                Ok(())
            }
        }
    }
}

/// What the app still does for an event the draft applied.
enum Reaction {
    /// Refuse the tool call the warm session made.
    Refuse(ToolRequestId),
    /// The agent answered the create on another model than the wanted one, which it lists:
    /// set the wanted model on the warm session.
    SetModel(ConfigValueId),
}

impl Draft {
    /// Applies `event` of the warm session, as [`App::apply_event`] describes, and returns
    /// what the app still does for it.
    fn apply(&mut self, event: BackendEvent) -> Option<Reaction> {
        match event {
            BackendEvent::SessionCreated {
                request,
                backend_session,
                config,
                ..
            } if request == self.request => {
                let model = model_to_set(&config, self.settings.model.as_ref());
                self.phase = Phase::Ready {
                    backend_session,
                    config,
                    switching: None,
                };
                return model.map(Reaction::SetModel);
            }
            BackendEvent::RequestFailed { request, error, .. } if request == self.request => {
                self.phase = Phase::Failed(error);
            }
            BackendEvent::SessionClosed { error, .. } => {
                self.phase = Phase::Failed(format!("connection to the agent closed: {error}"));
            }
            BackendEvent::Update {
                update: Update::ConfigOptions(options),
                ..
            } => {
                if let Phase::Ready {
                    config, switching, ..
                } = &mut self.phase
                {
                    *config = options;
                    *switching = None;
                }
            }
            BackendEvent::Update {
                update:
                    Update::ConfigRejected {
                        option,
                        value,
                        error,
                    },
                ..
            } => {
                self.reject_model(&option, &value);
                self.held.push(Update::ConfigRejected {
                    option,
                    value,
                    error,
                });
            }
            BackendEvent::Update { update, .. } => self.held.push(update),
            BackendEvent::ToolCalled { call, .. } => return Some(Reaction::Refuse(call)),
            BackendEvent::SessionCreated { .. }
            | BackendEvent::RequestFailed { .. }
            | BackendEvent::SessionLoaded { .. } => {}
        }
        None
    }

    /// Puts the model back to the one the agent runs when the agent rejected the change of
    /// `option`, the agent's model option, to `value`, the latest model picked, while a model
    /// change is on its way: the agent's efforts are that model's, and the wanted effort is
    /// checked against them. The rejection of an earlier pick leaves the latest one on its
    /// way.
    fn reject_model(&mut self, option: &ConfigId, value: &ConfigValueId) {
        if let Phase::Ready {
            config, switching, ..
        } = &mut self.phase
            && let Some(model) = config
                .iter_mut()
                .find(|candidate| candidate.category == ConfigCategory::Model)
            && model.id == *option
            && model.current == *value
            && let Some(running) = switching.take()
        {
            model.current = running.clone();
            self.settings.model = Some(running);
        }
    }

    /// The session creating this draft makes, working in `workspace`: in a worktree, the
    /// worktree is its folder. An orchestrator records the branch settings for its children.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::DraftFailed`] when the warm session could not start.
    fn session(&self, workspace: Workspace) -> Result<Session, AppError> {
        let settings = &self.settings;
        let (backend_session, model, effort) = match &self.phase {
            Phase::Failed(_) => return Err(AppError::DraftFailed),
            Phase::Loading => (None, settings.model.clone(), settings.effort.clone()),
            Phase::Ready {
                backend_session,
                config,
                switching,
            } => (
                Some(backend_session.clone()),
                selection(config, &ConfigCategory::Model, settings.model.as_ref()),
                if switching.is_some() {
                    settings.effort.clone()
                } else {
                    selection(config, &ConfigCategory::Effort, settings.effort.as_ref())
                },
            ),
        };
        Ok(Session {
            id: self.id.clone(),
            project: settings.project.clone(),
            scope: settings.scope.clone(),
            cwd: match &workspace {
                Workspace::Worktree { path, .. } => path.clone(),
                Workspace::CheckedOut | Workspace::NewBranch { .. } => settings.cwd.clone(),
            },
            title: NEW_SESSION_TITLE.into(),
            backend_session,
            backend: settings.backend.clone(),
            model,
            effort,
            status: Status::Idle,
            parent: None,
            auto_allow: settings.auto_allow,
            instructions_injected: false,
            created_at: unix_seconds(),
            orchestrator: settings.orchestrator,
            archived: None,
            workspace,
            child_defaults: settings
                .orchestrator
                .then(|| settings.branch.child_defaults()),
        })
    }

    /// The picker of `category`, showing `wanted` where the agent lists it.
    fn choice(&self, category: &ConfigCategory, wanted: Option<&ConfigValueId>) -> DraftChoice {
        let Phase::Ready { config, .. } = &self.phase else {
            return DraftChoice::Waiting(wanted.cloned());
        };
        let option = config.iter().find(|option| &option.category == category);
        match (option, selection(config, category, wanted)) {
            (Some(option), Some(current)) => DraftChoice::Offered(option_view(option, current)),
            _ => DraftChoice::NotOffered,
        }
    }

    /// How far the warm session has started.
    fn warmup(&self) -> Warmup {
        match &self.phase {
            Phase::Loading => Warmup::Loading,
            Phase::Ready { .. } => Warmup::Ready,
            Phase::Failed(error) => Warmup::Failed(error.clone()),
        }
    }
}

/// The value of the first option of `category` in `config` a session records: `wanted` when
/// the option lists it among its choices, else the option's current value. `None` when no
/// option has the category.
fn selection(
    config: &[ConfigOption],
    category: &ConfigCategory,
    wanted: Option<&ConfigValueId>,
) -> Option<ConfigValueId> {
    let option = config.iter().find(|option| &option.category == category)?;
    let listed = wanted.filter(|wanted| option.choices.iter().any(|choice| choice.id == **wanted));
    Some(listed.unwrap_or(&option.current).clone())
}

/// The `wanted` model when `config` lists it but runs another one.
fn model_to_set(config: &[ConfigOption], wanted: Option<&ConfigValueId>) -> Option<ConfigValueId> {
    let model = config
        .iter()
        .find(|option| option.category == ConfigCategory::Model)?;
    selection(config, &ConfigCategory::Model, wanted).filter(|wanted| *wanted != model.current)
}

#[cfg(test)]
mod tests;
