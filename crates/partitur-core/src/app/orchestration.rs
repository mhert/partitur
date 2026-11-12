//! How Partitur talks to an orchestrator on its own: the orchestrator's tool calls are
//! answered, a child's finished turn reaches its parent as a prompt, and prompts Partitur
//! generates for a busy session wait for its turn to end.
//!
//! The approval card of a proposed split gives every child branch settings of its own, which
//! start from the orchestrator's. Each child is checked in its own repository: the
//! repositories of the children that make a branch are read once per proposal, in the
//! background, and read again after an approval ended. Approving a split is all or nothing:
//! the git work of every child runs first, as one request, and only once all of it succeeded
//! are the children started; when a step fails, whatever the approval made is undone and the
//! proposal stays undecided.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use super::git_work::Work;
use super::workspace::{
    CheckedChild, Context, Planned, RepoState, SplitChild, check_children, create_steps,
    is_loading, repo_state, undo_steps, workspace_of,
};
use super::{App, AppError, SessionChoices};
use crate::backend::{
    ApprovedChild, ChildOutcome, Decision, OrchestratorTool, ProposedChild, TurnOrigin, Update,
};
use crate::git::{GitAnswer, GitError, GitQuery, GitStep};
use crate::ids::{BackendId, GitRequestId, ProposalId, RepositoryId, SessionId};
use crate::orchestrator::{
    PROPOSAL_RECORDED, child_result_prompt, children_started_prompt, rejection_prompt,
};
use crate::project::{Project, Repository};
use crate::session::{BranchChoice, BranchMode, Scope, Status, Workspace, title_from_prompt};
use crate::settings::ProjectSettings;
use crate::tool_call::ToolCall;
use crate::transcript::{Entry, Transcript};
use crate::view_model::{ApprovalView, ChildCheck};

/// The word the child list shows for a session's status.
fn status_word(status: Status) -> &'static str {
    match status {
        Status::Idle => "idle",
        Status::Running => "running",
        Status::WaitingForYou => "waiting for you",
        Status::Interrupted => "interrupted",
        Status::Done => "done",
        Status::Failed => "failed",
    }
}

/// What a child reports when its turn ended without an answer from the agent.
const NO_FINAL_MESSAGE: &str = "(no final message)";

/// The answer to a tool call from a session that does not exist or is not attached.
const UNKNOWN_SESSION: &str = "unknown session";

/// The answer to a tool call from a session that was deleted.
const DELETED_SESSION: &str = "the session was deleted";

/// The answer to a tool call from a session that is not an orchestrator.
const NOT_AN_ORCHESTRATOR: &str = "this session is not an orchestrator";

/// The answer to a `propose_children` call without children.
const NO_CHILDREN: &str = "propose_children needs at least one child";

/// The answer to a `send_to_child` call with a blank prompt.
const NO_PROMPT: &str = "send_to_child needs a prompt";

/// Why a split cannot be approved while a child's repository has not been read yet.
const STILL_LOADING: &str = "the branches are still loading";

/// Why the card of a split takes no decision while its approval is in flight.
const BEING_APPROVED: &str = "the split is being approved";

/// One child as the approval card leaves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildChoice {
    /// The repository the child works in.
    pub repository: RepositoryId,
    /// The child's first prompt.
    pub prompt: String,
    /// The child's display title; a blank one is derived from the prompt.
    pub title: String,
    /// Whether the child grants permission requests without asking.
    pub auto_allow: bool,
    /// How the child uses its repository's git; the card starts it at the orchestrator's
    /// [`child_defaults`](crate::Session::child_defaults).
    pub branch: BranchChoice,
}

/// What [`App::approve_proposal`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approval {
    /// The children were started, these sessions in order.
    Started(Vec<SessionId>),
    /// The git work of the children is queued; the children start once all of it succeeded.
    Pending,
}

/// The approval card state of one undecided proposal.
#[derive(Debug)]
pub(super) struct SplitState {
    /// The orchestrator that proposed it.
    session: SessionId,
    /// What git answered about each child repository read so far, by folder.
    reads: HashMap<PathBuf, RepoState>,
    /// The git request of the approval in flight, `None` while none is.
    approving: Option<GitRequestId>,
    /// Why the last approval failed.
    error: Option<String>,
}

impl SplitState {
    /// The orchestrator that proposed the split.
    pub(super) fn session(&self) -> &SessionId {
        &self.session
    }
}

/// An approval on its way: the split, its children as approved, and each child's git work,
/// `None` for a Checked-out child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Approving {
    /// The orchestrator.
    session: SessionId,
    /// The proposal approved.
    proposal: ProposalId,
    /// The children, blank titles derived from the prompts.
    children: Vec<ChildChoice>,
    /// The git work of each child, in child order.
    plans: Vec<Option<Planned>>,
}

impl Approving {
    /// The repositories the children's git work happens in, each once, in child order.
    fn repositories(&self) -> Vec<PathBuf> {
        let mut repositories: Vec<PathBuf> = Vec::new();
        for planned in self.plans.iter().flatten() {
            if !repositories.contains(&planned.repository.path) {
                repositories.push(planned.repository.path.clone());
            }
        }
        repositories
    }
}

/// A prompt Partitur writes to a session on its own, not typed by the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Generated {
    /// Wakes an orchestrator, for example with the decision on its proposal.
    Wake(String),
    /// A prompt an orchestrator sends one of its children.
    FromOrchestrator(String),
    /// A child's finished turn, reported to its orchestrator.
    ChildResult {
        /// The child session.
        child: SessionId,
        /// The repository the child works in.
        repository: RepositoryId,
        /// The child's display title.
        title: String,
        /// Who started the child's turn.
        origin: TurnOrigin,
        /// How the child's turn ended.
        outcome: ChildOutcome,
        /// The child's final answer of the turn.
        text: String,
    },
}

impl Generated {
    /// The update that records the prompt in the receiving session's transcript.
    fn record(&self) -> Update {
        match self {
            Self::Wake(text) => Update::PartiturPrompt(text.clone()),
            Self::FromOrchestrator(text) => Update::OrchestratorPrompt(text.clone()),
            Self::ChildResult {
                child,
                repository,
                title,
                origin,
                outcome,
                text,
            } => Update::ChildResult {
                child: child.clone(),
                repository: repository.clone(),
                title: title.clone(),
                origin: *origin,
                outcome: outcome.clone(),
                text: text.clone(),
            },
        }
    }

    /// The text the agent is prompted with.
    fn text(&self) -> String {
        match self {
            Self::Wake(text) | Self::FromOrchestrator(text) => text.clone(),
            Self::ChildResult {
                child,
                repository,
                title,
                origin,
                outcome,
                text,
            } => child_result_prompt(child, repository, title, *origin, outcome, text),
        }
    }
}

impl App {
    /// Checks the children of the undecided proposal `proposal` of the orchestrator `session`
    /// as the approval card leaves them, `children`, and returns one check per child, in
    /// order.
    ///
    /// The project's settings are read again on every call. Each child is checked in its own
    /// repository as the new-session sheet checks one, with what git answered about it: a
    /// base the repository lacks, or none, becomes the branch checked out there. Two Worktree
    /// or New branch children may not want one branch name in one repository, nor two children
    /// one worktree folder, and a New branch child may not share its checkout with a live
    /// session or with another child that works there (Checked-out or New branch).
    ///
    /// A Checked-out child needs no reading. The first call for a proposal, and the first
    /// after its approval ended, queues one read-only git request with an `Inspect` of every
    /// other child repository not read yet; its children are loading until the answer arrives
    /// through [`apply_git_event`](Self::apply_git_event). A repository busy with a changing
    /// request, such as the undo of a failed approval, is not read until it is free, so the
    /// reading shows what that request left; its children are loading meanwhile. A reading
    /// that failed is kept and shown until the user changes the card, which calls this again
    /// and reads it again, or until a changing request in its repository ends. An answer for
    /// a reading the proposal no longer waits for is ignored.
    ///
    /// This is the check of a change the user made to the card; checks of the card that only
    /// take in what git answered and how the sessions changed use
    /// [`recheck_split`](Self::recheck_split).
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::UnknownProposal`] when its loaded transcript has no undecided proposal
    /// `proposal` and [`AppError::Store`] when the project's settings cannot be read; nothing
    /// is read then.
    pub fn check_split(
        &mut self,
        session: &SessionId,
        proposal: &ProposalId,
        children: &[ChildChoice],
    ) -> Result<Vec<ChildCheck>, AppError> {
        let checked = self.split_checked(session, proposal, children, true)?;
        Ok(checked.into_iter().map(|child| child.check).collect())
    }

    /// Checks the children of the undecided proposal `proposal` of the orchestrator `session`
    /// as [`check_split`](Self::check_split) does, but keeps the readings that failed, so
    /// checking the card after every git answer and every change of the sessions never reads
    /// a repository again that git just failed to read.
    ///
    /// # Errors
    ///
    /// As [`check_split`](Self::check_split).
    pub fn recheck_split(
        &mut self,
        session: &SessionId,
        proposal: &ProposalId,
        children: &[ChildChoice],
    ) -> Result<Vec<ChildCheck>, AppError> {
        let checked = self.split_checked(session, proposal, children, false)?;
        Ok(checked.into_iter().map(|child| child.check).collect())
    }

    /// Approves the undecided proposal `proposal` of the orchestrator `session` with
    /// `children` as the approval card leaves them.
    ///
    /// Approval goes in this order:
    ///
    /// 1. The proposal must be undecided and not being approved already, and the children
    ///    valid: at least one, each in one of the project's repositories with a prompt.
    /// 2. The children are checked as [`check_split`](Self::check_split) checks them; a child
    ///    with a problem, or one whose repository has not been read yet, refuses the approval.
    /// 3. When no child makes a branch, the children start at once and
    ///    [`Approval::Started`] is returned.
    /// 4. Otherwise [`Approval::Pending`] is returned, and the proposal is being approved
    ///    from now on: its card takes neither another approval nor a rejection, and its
    ///    repositories are busy until the approval ends, its undo included. The
    ///    children's repositories are read again, and the children checked against that
    ///    fresh reading: a problem, or work that differs from what the card showed, ends the
    ///    approval with the reason. Otherwise one git request runs, keeping its repositories
    ///    busy: a fetch for each repository and remote wanted, then each child's branch or
    ///    worktree, in child order. A step that fails ends the approval with its error, and
    ///    what the approval made so far is undone, the last first. Once every step succeeded,
    ///    the children start; when the orchestrator was deleted or archived meanwhile, what
    ///    the approval made is undone instead and nothing starts. The heads the undo returns
    ///    to come from the fresh reading.
    ///
    /// An approval that ended without starting the children leaves the proposal undecided,
    /// with the reason in [`SessionViewModel::approval`](crate::SessionViewModel::approval),
    /// and the repositories are read again for the next check. The next approval clears the
    /// reason.
    ///
    /// The children start one by one. Each is a new session in its repository, or in its
    /// worktree, a child of `session` running on the orchestrator's backend (on the
    /// [default backend](Self::default_backend) when that is no longer configured) with the
    /// orchestrator's model and effort (with neither on the default backend, whose agent
    /// does not know the orchestrator's ids), titled as chosen (a blank title is derived from
    /// the prompt), granting permissions without asking when chosen and recording the
    /// workspace it works in; it is not selected. It is [`Running`](Status::Running) at once,
    /// and its prompt goes out, after its repository's instruction chain, as soon as its
    /// backend session is created. Once every child is started, the decision goes into the
    /// orchestrator's transcript, and the orchestrator is woken with the started children:
    /// their session ids, modes, branches and folders. It is woken at once when it is idle or
    /// was waiting only for its proposals, else once its turn ends, and always after child
    /// results that arrived before the decision.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::Archived`] when it is archived, [`AppError::UnknownProposal`] when the
    /// session's loaded transcript has no undecided proposal `proposal`,
    /// [`AppError::Workspace`] (`the split is being approved`) while its approval is in
    /// flight, [`AppError::InvalidChildren`] when `children` is empty, names a repository
    /// that is not one of the project's, has a blank prompt, a child with a problem, or one
    /// still loading (`the branches are still loading`), [`AppError::Git`] when the git port
    /// cannot queue the reading, and [`AppError::Store`] when the settings cannot be read
    /// (nothing changes in these cases). Starting the children returns
    /// [`AppError::Backend`] when the backend cannot queue a command and [`AppError::Store`]
    /// when the change cannot be saved: the children started so far stay in place with
    /// their workspaces, what git made for the others is undone, and the proposal stays
    /// undecided, so approving it again starts those children a second time. After a
    /// pending approval, these errors come from
    /// [`apply_git_event`](Self::apply_git_event).
    pub fn approve_proposal(
        &mut self,
        session: SessionId,
        proposal: ProposalId,
        children: Vec<ChildChoice>,
    ) -> Result<Approval, AppError> {
        self.check_undecided(&session, &proposal)?;
        self.check_not_approving(&proposal)?;
        let project = self.session(&session)?.project.clone();
        let children = titled_children(self.project(&project)?, children)?;
        let checked = self.split_checked(&session, &proposal, &children, true)?;
        if let Some(problem) = checked.iter().find_map(|child| child.check.problem.clone()) {
            return Err(AppError::InvalidChildren(problem));
        }
        if checked.iter().any(|child| child.check.loading) {
            return Err(AppError::InvalidChildren(STILL_LOADING.into()));
        }
        let approving = Approving {
            session,
            proposal,
            children,
            plans: checked.into_iter().map(|child| child.planned).collect(),
        };
        if approving.plans.iter().all(Option::is_none) {
            return self.start_children(approving).map(Approval::Started);
        }
        let read = approving
            .repositories()
            .into_iter()
            .map(GitQuery::Inspect)
            .collect();
        let proposal = approving.proposal.clone();
        let held = approving.repositories();
        let request =
            self.git_work
                .query_holding(&*self.git, Work::ApprovalCheck(approving), read, held)?;
        if let Some(split) = self.splits.get_mut(&proposal) {
            split.approving = Some(request);
            split.error = None;
        }
        Ok(Approval::Pending)
    }

    /// Rejects the undecided proposal `proposal` of the orchestrator `session` with the
    /// user's `note` (a blank one counts as none). The rejection goes into the orchestrator's
    /// transcript and wakes it with the note, as
    /// [`approve_proposal`](Self::approve_proposal) wakes it with the started children.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::Archived`] when it is archived, [`AppError::UnknownProposal`] when the
    /// session's loaded transcript has no undecided proposal `proposal`,
    /// [`AppError::Workspace`] (`the split is being approved`) while its approval is in
    /// flight (nothing changes in these cases), [`AppError::Backend`] when the backend
    /// cannot queue the prompt and [`AppError::Store`] when the change cannot be saved.
    pub fn reject_proposal(
        &mut self,
        session: SessionId,
        proposal: ProposalId,
        note: Option<String>,
    ) -> Result<(), AppError> {
        self.check_undecided(&session, &proposal)?;
        self.check_not_approving(&proposal)?;
        let note = note.filter(|note| !note.trim().is_empty());
        let wake = rejection_prompt(note.as_deref());
        self.decide(session, proposal, Decision::Rejected { note }, wake)
    }

    /// The backend the children of the orchestrator `session` run on: its own when it is
    /// configured, else the default.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists.
    fn child_backend(&self, session: &SessionId) -> Result<BackendId, AppError> {
        let backend = &self.session(session)?.backend;
        if self.is_configured(backend) {
            Ok(backend.clone())
        } else {
            Ok(self.default.clone())
        }
    }

    /// Fails unless the session is live and its loaded transcript has the undecided proposal
    /// `proposal`.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::Archived`] when it is archived and [`AppError::UnknownProposal`] when the
    /// proposal is not undecided there.
    fn check_undecided(&self, session: &SessionId, proposal: &ProposalId) -> Result<(), AppError> {
        if self.is_archived(session)? {
            return Err(AppError::Archived(session.clone()));
        }
        self.check_proposed(session, proposal)
    }

    /// Fails unless the session exists and its loaded transcript has the undecided proposal
    /// `proposal`.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists and
    /// [`AppError::UnknownProposal`] when the proposal is not undecided there.
    fn check_proposed(&self, session: &SessionId, proposal: &ProposalId) -> Result<(), AppError> {
        self.session(session)?;
        let undecided = self
            .transcripts
            .get(session)
            .and_then(|transcript| transcript.pending_proposal(proposal))
            .is_some();
        if undecided {
            Ok(())
        } else {
            Err(AppError::UnknownProposal(proposal.clone()))
        }
    }

    /// Fails while the approval of `proposal` is in flight.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Workspace`] (`the split is being approved`) then.
    fn check_not_approving(&self, proposal: &ProposalId) -> Result<(), AppError> {
        if self
            .splits
            .get(proposal)
            .is_some_and(|split| split.approving.is_some())
        {
            return Err(AppError::Workspace(BEING_APPROVED.into()));
        }
        Ok(())
    }

    /// Reads the repositories of `children` the proposal has not read yet, as
    /// [`check_split`](Self::check_split) describes, and checks every child; with `retry`, the
    /// readings that failed are read again first.
    ///
    /// # Errors
    ///
    /// As [`check_split`](Self::check_split).
    fn split_checked(
        &mut self,
        session: &SessionId,
        proposal: &ProposalId,
        children: &[ChildChoice],
        retry: bool,
    ) -> Result<Vec<CheckedChild>, AppError> {
        self.check_proposed(session, proposal)?;
        let project = self.project(&self.session(session)?.project)?.clone();
        let settings = self.store.load_settings(project.id.clone())?;
        let split = self
            .splits
            .entry(proposal.clone())
            .or_insert_with(|| SplitState {
                session: session.clone(),
                reads: HashMap::new(),
                approving: None,
                error: None,
            });
        if retry {
            split
                .reads
                .retain(|_, state| !matches!(state, RepoState::Failed(_)));
        }
        let mut unread: Vec<PathBuf> = Vec::new();
        for child in children
            .iter()
            .filter(|child| child.branch.mode != BranchMode::CheckedOut)
        {
            if let Some(repository) = child_repository(&project, &child.repository)
                && !split.reads.contains_key(&repository.path)
                && !unread.contains(&repository.path)
                && !self.git_work.is_busy(&repository.path)
            {
                unread.push(repository.path.clone());
            }
        }
        if !unread.is_empty() {
            let queries = unread.iter().cloned().map(GitQuery::Inspect).collect();
            let work = Work::SplitInspect {
                proposal: proposal.clone(),
                repositories: unread.clone(),
            };
            let state = match self.git_work.query(&*self.git, work, queries) {
                Ok(request) => RepoState::Loading(request),
                Err(error) => RepoState::Failed(error.to_string()),
            };
            for repository in unread {
                split.reads.insert(repository, state.clone());
            }
        }
        Ok(self.checked(&project, &settings, proposal, children))
    }

    /// Forgets the failed readings of `freed`, repositories no changing request keeps busy any
    /// more, so the next check of a card reads them again.
    pub(super) fn forget_failed_reads(&mut self, freed: &[PathBuf]) {
        for split in self.splits.values_mut() {
            split.reads.retain(|repository, state| {
                !(matches!(state, RepoState::Failed(_)) && freed.contains(repository))
            });
        }
    }

    /// Checks every child of `children` with what the proposal has read so far.
    fn checked(
        &self,
        project: &Project,
        settings: &ProjectSettings,
        proposal: &ProposalId,
        children: &[ChildChoice],
    ) -> Vec<CheckedChild> {
        let unread = HashMap::new();
        let reads = self
            .splits
            .get(proposal)
            .map_or(&unread, |split| &split.reads);
        let children: Vec<SplitChild<'_>> = children
            .iter()
            .map(|child| SplitChild {
                id: &child.repository,
                repository: child_repository(project, &child.repository),
                choice: &child.branch,
            })
            .collect();
        let busy = |repository: &Path| self.git_work.is_busy(repository);
        let context = Context {
            sessions: &self.sessions,
            global: &self.global,
            busy: &busy,
        };
        check_children(project, settings, &children, reads, &context)
    }

    /// Applies the `answers` git gave to `request`, which read `repositories` for the card
    /// of `proposal`, to each repository the proposal still waits for this request for.
    pub(super) fn split_inspected(
        &mut self,
        request: GitRequestId,
        proposal: &ProposalId,
        repositories: Vec<PathBuf>,
        answers: Vec<Result<GitAnswer, GitError>>,
    ) {
        let Some(split) = self.splits.get_mut(proposal) else {
            return;
        };
        let mut answers = answers.into_iter();
        for repository in repositories {
            let answer = answers.next();
            if split
                .reads
                .get(&repository)
                .is_some_and(|state| is_loading(state, request))
            {
                split.reads.insert(repository, repo_state(answer));
            }
        }
    }

    /// Checks the approval `pressed` against the fresh reading `answers` of its
    /// repositories, and queues its git work when nothing stops it, as
    /// [`approve_proposal`](Self::approve_proposal) describes. An approval whose orchestrator
    /// was deleted meanwhile is gone; one whose orchestrator was archived ends.
    pub(super) fn approval_checked(
        &mut self,
        pressed: Approving,
        answers: Vec<Result<GitAnswer, GitError>>,
    ) {
        let proposal = pressed.proposal.clone();
        let Some(split) = self.splits.get_mut(&proposal) else {
            return;
        };
        let mut answers = answers.into_iter();
        for repository in pressed.repositories() {
            split.reads.insert(repository, repo_state(answers.next()));
        }
        if self.is_archived(&pressed.session).unwrap_or(true) {
            self.end_approval(&proposal, None);
            return;
        }
        let fresh = match self.fresh_plans(&pressed) {
            Ok(fresh) => fresh,
            Err(error) => {
                self.end_approval(&proposal, Some(error));
                return;
            }
        };
        let approving = Approving {
            plans: fresh,
            ..pressed
        };
        let (steps, fetches) = split_steps(&approving.plans);
        let work = Work::Approve { approving, fetches };
        match self.git_work.run(&*self.git, work, steps) {
            Ok(request) => {
                if let Some(split) = self.splits.get_mut(&proposal) {
                    split.approving = Some(request);
                }
            }
            Err(error) => self.end_approval(&proposal, Some(error.to_string())),
        }
    }

    /// The git work of `pressed`, planned again from the proposal's reading as it is now.
    ///
    /// # Errors
    ///
    /// Returns why the approval cannot go on: the settings cannot be read, a child has a
    /// problem, or a child's work differs from what the card showed.
    fn fresh_plans(&self, pressed: &Approving) -> Result<Vec<Option<Planned>>, String> {
        let project = self
            .session(&pressed.session)
            .and_then(|session| self.project(&session.project))
            .map_err(|error| error.to_string())?;
        let settings = self
            .store
            .load_settings(project.id.clone())
            .map_err(|error| error.to_string())?;
        let checked = self.checked(project, &settings, &pressed.proposal, &pressed.children);
        if let Some(problem) = checked.iter().find_map(|child| child.check.problem.clone()) {
            return Err(problem);
        }
        let fresh: Vec<Option<Planned>> = checked.into_iter().map(|child| child.planned).collect();
        let changed =
            pressed
                .plans
                .iter()
                .zip(&fresh)
                .find_map(|(shown, now)| match (shown, now) {
                    (Some(shown), Some(now)) if shown.same_work(now) => None,
                    (Some(shown), _) => Some(shown.repository.name.clone()),
                    (None, _) => None,
                });
        match changed {
            Some(repository) => Err(format!(
                "{repository} changed meanwhile; check the branch settings and approve again"
            )),
            None => Ok(fresh),
        }
    }

    /// Finishes the approval `approving`, whose git work ran `done` steps, `fetches` of them
    /// fetches first, with `result`, as [`approve_proposal`](Self::approve_proposal)
    /// describes.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Undo`] when an undo cannot be queued, and the
    /// [`AppError::Backend`] or [`AppError::Store`] errors of starting the children.
    pub(super) fn approval_ran(
        &mut self,
        approving: Approving,
        fetches: usize,
        done: usize,
        result: Result<(), GitError>,
    ) -> Result<(), AppError> {
        let made: Vec<Planned> = approving
            .plans
            .iter()
            .flatten()
            .take(done.saturating_sub(fetches))
            .cloned()
            .collect();
        let error = result.err().map(|error| error.to_string());
        let failed = error.is_some();
        self.end_approval(&approving.proposal, error);
        let live = self
            .session(&approving.session)
            .is_ok_and(|session| session.archived.is_none());
        if failed || !live {
            return self.undo_split(&made);
        }
        self.start_children(approving).map(drop)
    }

    /// Ends the approval of `proposal` in flight, with the reason it failed, if any; the
    /// proposal's repositories are read again by the next check.
    fn end_approval(&mut self, proposal: &ProposalId, error: Option<String>) {
        if let Some(split) = self.splits.get_mut(proposal) {
            split.approving = None;
            split.reads.clear();
            split.error = error;
        }
    }

    /// Queues the undo of what the git work of `made` made, the last first, as one request.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Undo`] when the git port cannot queue it.
    fn undo_split(&mut self, made: &[Planned]) -> Result<(), AppError> {
        let mut steps = Vec::new();
        let mut repositories = Vec::new();
        for planned in made.iter().rev() {
            let undo = undo_steps(
                &planned.repository.path,
                &workspace_of(planned),
                &planned.before,
            );
            repositories.extend(undo.iter().map(|_| planned.repository.name.clone()));
            steps.extend(undo);
        }
        if steps.is_empty() {
            return Ok(());
        }
        self.queue_undo(steps, repositories)
    }

    /// Starts the children of `approving` with their workspaces, records the decision and
    /// wakes the orchestrator, as [`approve_proposal`](Self::approve_proposal) describes;
    /// returns the children's sessions in order. When a child cannot be started, what git
    /// made for the children not created yet is undone.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when the orchestrator does not exist,
    /// [`AppError::Backend`] when the backend cannot queue a command and [`AppError::Store`]
    /// when the change cannot be saved.
    fn start_children(&mut self, approving: Approving) -> Result<Vec<SessionId>, AppError> {
        let Approving {
            session,
            proposal,
            children,
            plans,
        } = approving;
        let mut created = 0;
        let approved = match self.start_each(&session, &children, &plans, &mut created) {
            Ok(approved) => approved,
            Err(error) => {
                let unmade: Vec<Planned> = plans.into_iter().skip(created).flatten().collect();
                // The failure that is returned matters more than whether its undo queued.
                drop(self.undo_split(&unmade));
                return Err(error);
            }
        };
        let started = approved.iter().map(|child| child.session.clone()).collect();
        let wake = children_started_prompt(&approved);
        self.decide(session, proposal, Decision::Approved(approved), wake)?;
        Ok(started)
    }

    /// Creates and prompts each of `children` as a child of `session`, working in the
    /// workspace its plan in `plans` makes; `created` counts the sessions created so far.
    ///
    /// # Errors
    ///
    /// As [`start_children`](Self::start_children).
    fn start_each(
        &mut self,
        session: &SessionId,
        children: &[ChildChoice],
        plans: &[Option<Planned>],
        created: &mut usize,
    ) -> Result<Vec<ApprovedChild>, AppError> {
        let project = self.session(session)?.project.clone();
        let backend = self.child_backend(session)?;
        let orchestrator = self.session(session)?;
        let (model, effort) = if backend == orchestrator.backend {
            (orchestrator.model.clone(), orchestrator.effort.clone())
        } else {
            (None, None)
        };
        let mut approved = Vec::new();
        for (child, planned) in children.iter().zip(plans) {
            let workspace = planned.as_ref().map_or(Workspace::CheckedOut, workspace_of);
            let id = self.create_session(
                project.clone(),
                Scope::Repository(child.repository.clone()),
                Some(session.clone()),
                child.title.clone(),
                SessionChoices {
                    backend: backend.clone(),
                    model: model.clone(),
                    effort: effort.clone(),
                    auto_allow: child.auto_allow,
                },
                workspace.clone(),
            )?;
            *created += 1;
            let cwd = self.session(&id)?.cwd.clone();
            let record = Update::OrchestratorPrompt(child.prompt.clone());
            let text = child.prompt.clone();
            self.start_turn(&id, &record, text, Vec::new(), TurnOrigin::Orchestrator)?;
            approved.push(ApprovedChild {
                session: id,
                repository: child.repository.clone(),
                prompt: child.prompt.clone(),
                title: child.title.clone(),
                auto_allow: child.auto_allow,
                mode: workspace.mode(),
                branch: workspace.branch().map(str::to_owned),
                cwd,
            });
        }
        Ok(approved)
    }

    /// Where the approval of one of the orchestrator `session`'s proposals stands, when one
    /// is in flight or failed last; an approval in flight comes first.
    pub(super) fn approval_view(&self, session: &SessionId) -> Option<ApprovalView> {
        self.splits
            .iter()
            .filter(|(_, split)| {
                split.session == *session && (split.approving.is_some() || split.error.is_some())
            })
            .map(|(proposal, split)| ApprovalView {
                proposal: proposal.clone(),
                pending: split.approving.is_some(),
                error: split.error.clone(),
            })
            .max_by_key(|view| view.pending)
    }

    /// Records `decision` on the orchestrator `session`'s proposal `proposal` and wakes it
    /// with `wake`. An orchestrator that waited only for its proposals becomes
    /// [`Idle`](Status::Idle) first, so the wake goes out now; when older generated prompts
    /// wait for an orchestrator that is free, the oldest goes out instead and the wake queues
    /// behind them.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::Backend`] when the backend cannot queue the prompt and [`AppError::Store`]
    /// when the change cannot be saved.
    fn decide(
        &mut self,
        session: SessionId,
        proposal: ProposalId,
        decision: Decision,
        wake: String,
    ) -> Result<(), AppError> {
        let decided = Update::ProposalDecided {
            id: proposal.clone(),
            decision,
        };
        self.apply_update(&session, &decided)?;
        self.splits.remove(&proposal);
        let undecided = (session, proposal);
        self.proposals.remove(&undecided);
        let (session, _) = undecided;
        let waited_for_proposals = self.session(&session)?.status == Status::WaitingForYou
            && self.waiting_permissions(&session).is_empty()
            && !self.awaits_decision(&session);
        if waited_for_proposals {
            self.change_session(session.clone(), |woken| woken.status = Status::Idle)?;
        }
        if !self.deliver(&session, Generated::Wake(wake))? && !self.in_turn(&session)? {
            self.deliver_next(&session)?;
        }
        Ok(())
    }

    /// Handles an orchestrator's tool call: validates it against the core's state, records
    /// what it changes and answers through the call's reply. Never fails on a bad call: the
    /// orchestrator gets the error text.
    ///
    /// A call whose caller stopped waiting, checked when the call is taken up, changes
    /// nothing. A call from a session that does not exist or is not attached is answered with
    /// `unknown session`, one from a session that is not an orchestrator with `this session
    /// is not an orchestrator`; neither changes anything. A call whose handling fails is
    /// answered with the failure's text before the failure is returned, so the orchestrator
    /// does not wait for an answer that never comes.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Backend`] when a prompt cannot be queued and [`AppError::Store`]
    /// when the change cannot be saved.
    pub(super) fn tool_call_received(&mut self, call: ToolCall) -> Result<(), AppError> {
        if !call.reply.is_waiting() {
            return Ok(());
        }
        let ToolCall {
            session,
            tool,
            reply,
        } = call;
        let handled = match (self.tool_refusal(&session), tool) {
            (Some(refusal), _) => Ok(Err(refusal.to_owned())),
            (None, OrchestratorTool::ProposeChildren(children)) => self.propose(&session, children),
            (None, OrchestratorTool::SendToChild { child, prompt }) => {
                self.send_to_child(&session, &child, prompt)
            }
            (None, OrchestratorTool::ListChildren) => Ok(Ok(self.list_children(&session))),
        };
        match handled {
            Ok(answer) => {
                reply.send(answer);
                Ok(())
            }
            Err(error) => {
                reply.send(Err(error.to_string()));
                Err(error)
            }
        }
    }

    /// Why a tool call from `session` is refused before looking at it, or `None` when the
    /// session is an attached orchestrator.
    fn tool_refusal(&self, session: &SessionId) -> Option<&'static str> {
        if self.deleted.contains(session) {
            return Some(DELETED_SESSION);
        }
        match self.session(session) {
            Ok(caller) if self.attached.contains(session) => {
                (!caller.orchestrator).then_some(NOT_AN_ORCHESTRATOR)
            }
            _ => Some(UNKNOWN_SESSION),
        }
    }

    /// `propose_children`: records the proposal in the orchestrator's transcript and among the
    /// undecided proposals, and answers at once without waiting for the user. A blank title is
    /// derived from the child's prompt.
    ///
    /// The call is refused, and nothing recorded, when it proposes no child, names a
    /// repository that is not one of the project's (the project root is none) or gives a
    /// child a blank prompt.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] or [`AppError::UnknownProject`] when the session
    /// or its project does not exist and [`AppError::Store`] when the proposal cannot be
    /// saved.
    fn propose(
        &mut self,
        session: &SessionId,
        children: Vec<ProposedChild>,
    ) -> Result<Result<String, String>, AppError> {
        let project = self.project(&self.session(session)?.project)?;
        let children = match checked_children(project, children) {
            Ok(children) => children,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let id = ProposalId::new();
        let proposal = Update::ChildrenProposed {
            id: id.clone(),
            children: children.clone(),
        };
        self.apply_update(session, &proposal)?;
        self.proposals.insert((session.clone(), id), children);
        Ok(Ok(PROPOSAL_RECORDED.to_owned()))
    }

    /// `send_to_child`: prompts the orchestrator's child `child` now, or queues the prompt
    /// until the child is free, as [`deliver`](Self::deliver) decides, and records in the
    /// orchestrator's transcript which child it prompted with what. A blank prompt is
    /// refused before the child is looked at, and so is a session that does not exist or is
    /// not a child of `session`; a refused call records nothing.
    ///
    /// # Errors
    ///
    /// As [`deliver`](Self::deliver), and [`AppError::Store`] when the record cannot be
    /// saved.
    fn send_to_child(
        &mut self,
        session: &SessionId,
        child: &SessionId,
        prompt: String,
    ) -> Result<Result<String, String>, AppError> {
        if prompt.trim().is_empty() {
            return Ok(Err(NO_PROMPT.to_owned()));
        }
        let Ok(target) = self.session(child) else {
            return Ok(Err(format!("unknown child {child}")));
        };
        if target.parent.as_ref() != Some(session) {
            return Ok(Err(format!("{child} is not a child of this session")));
        }
        let sent = self.deliver(child, Generated::FromOrchestrator(prompt.clone()))?;
        let prompted = Update::ChildPrompted {
            child: child.clone(),
            prompt,
        };
        self.apply_update(session, &prompted)?;
        let answer = if sent {
            format!("prompt sent to child {child}")
        } else {
            format!("prompt queued for child {child}; it goes out when the child is free")
        };
        Ok(Ok(answer))
    }

    /// `list_children`: one line per direct child of the orchestrator `session`, in session-list
    /// order, with its title, repository, session id and status; a note when it has none.
    fn list_children(&self, session: &SessionId) -> String {
        let lines: Vec<String> = self
            .sessions
            .iter()
            .filter_map(|child| match (&child.parent, &child.scope) {
                (Some(parent), Scope::Repository(repository)) if parent == session => {
                    Some(format!(
                        "- {} · {repository} · session {} · {}",
                        child.title,
                        child.id,
                        status_word(child.status)
                    ))
                }
                _ => None,
            })
            .collect();
        if lines.is_empty() {
            return "no children yet".to_owned();
        }
        format!("children:\n{}", lines.join("\n"))
    }

    /// Whether the session has a proposal the user has not decided yet.
    pub(super) fn awaits_decision(&self, session: &SessionId) -> bool {
        self.proposals
            .keys()
            .any(|(proposer, _)| proposer == session)
    }

    /// Starts a turn with `generated` when the session is not in a turn (neither running nor
    /// waiting for a permission answer; an orchestrator waiting only for the decision on its
    /// proposal is free), its project is open and no prompt is deferred for it yet, else queues
    /// it behind the prompts already deferred, so generated prompts go out in arrival order;
    /// returns whether it went out now. A session of a closed project never starts a turn on
    /// its own: its queue waits until the project is open again and a turn of the session
    /// ends.
    ///
    /// A generated turn counts as started by the orchestrator: a prompt from an orchestrator
    /// is, and the others go to orchestrators, which have no parent to report the origin to.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::Backend`] when the backend cannot queue the prompt and [`AppError::Store`]
    /// when the change cannot be saved.
    pub(super) fn deliver(
        &mut self,
        session: &SessionId,
        generated: Generated,
    ) -> Result<bool, AppError> {
        if !self.may_start_generated(session)? {
            self.deferred
                .entry(session.clone())
                .or_default()
                .push_back(generated);
            return Ok(false);
        }
        self.start_generated(session, &generated)?;
        Ok(true)
    }

    /// Sends the oldest prompt deferred for `session`, if any, unless the session is
    /// [parked](Self::parked).
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::Backend`] when the backend cannot queue the prompt and [`AppError::Store`]
    /// when the change cannot be saved.
    pub(super) fn deliver_next(&mut self, session: &SessionId) -> Result<(), AppError> {
        if self.parked(session)? {
            return Ok(());
        }
        let Some(generated) = self.deferred.get_mut(session).and_then(VecDeque::pop_front) else {
            return Ok(());
        };
        self.start_generated(session, &generated)
    }

    /// Settles the end of the session's turn, however it ended: the session reports
    /// `outcome` to its parent with the turn's origin (the user's when none was recorded),
    /// then, when `drain` is set, its next deferred prompt goes out.
    ///
    /// # Errors
    ///
    /// As [`report_to_parent`](Self::report_to_parent) and
    /// [`deliver_next`](Self::deliver_next).
    pub(super) fn settle_turn(
        &mut self,
        session: &SessionId,
        outcome: ChildOutcome,
        drain: bool,
    ) -> Result<(), AppError> {
        let origin = self
            .turn_origins
            .remove(session)
            .unwrap_or(TurnOrigin::User);
        self.report_to_parent(session, outcome, origin)?;
        if drain {
            self.deliver_next(session)?;
        }
        Ok(())
    }

    /// A child's turn that `origin` started ended with `outcome`: its final message goes to
    /// its parent. A session without a parent reports to nobody, and so does a
    /// project-scoped session, which is never a child.
    ///
    /// The parent's transcript is loaded before the result is delivered, so its undecided
    /// proposals from before a restart are known: the turn the result starts then ends
    /// waiting for you instead of done.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when `child` or its parent does not exist,
    /// [`AppError::Store`] when the child's or the parent's transcript cannot be loaded or the
    /// change cannot be saved and [`AppError::Backend`] when the backend cannot queue the
    /// prompt.
    pub(super) fn report_to_parent(
        &mut self,
        child: &SessionId,
        outcome: ChildOutcome,
        origin: TurnOrigin,
    ) -> Result<(), AppError> {
        let session = self.session(child)?;
        let (Some(parent), Scope::Repository(repository)) = (&session.parent, &session.scope)
        else {
            return Ok(());
        };
        let (parent, repository, title) =
            (parent.clone(), repository.clone(), session.title.clone());
        let result = Generated::ChildResult {
            child: child.clone(),
            repository,
            title,
            origin,
            outcome,
            text: final_message(self.ensure_transcript(child)?),
        };
        self.ensure_transcript(&parent)?;
        self.deliver(&parent, result)?;
        Ok(())
    }

    /// The session's transcript, loaded from the store when it is not loaded yet.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists and
    /// [`AppError::Store`] when its updates cannot be loaded.
    fn ensure_transcript(&mut self, id: &SessionId) -> Result<&Transcript, AppError> {
        self.load_transcript(id)?;
        self.transcripts
            .get(id)
            .ok_or_else(|| AppError::UnknownSession(id.clone()))
    }

    /// Whether a generated prompt may start a turn of the session now: it is not in a turn,
    /// it is not [parked](Self::parked), and no older generated prompt waits for it.
    fn may_start_generated(&self, session: &SessionId) -> Result<bool, AppError> {
        let nothing_deferred = self.deferred.get(session).is_none_or(VecDeque::is_empty);
        Ok(!self.in_turn(session)? && !self.parked(session)? && nothing_deferred)
    }

    /// Whether the session takes no generated prompt for now: its project is closed or the
    /// session is archived. Its generated prompts wait until it is reopened or restored.
    fn parked(&self, session: &SessionId) -> Result<bool, AppError> {
        let project = &self.session(session)?.project;
        Ok(self.project(project)?.closed || self.is_archived(session)?)
    }

    /// Starts a turn of `session` with `generated`, as [`deliver`](Self::deliver) describes.
    fn start_generated(
        &mut self,
        session: &SessionId,
        generated: &Generated,
    ) -> Result<(), AppError> {
        self.start_turn(
            session,
            &generated.record(),
            generated.text(),
            Vec::new(),
            TurnOrigin::Orchestrator,
        )
    }
}

/// The agent's last answer in the latest turn of `transcript`, or `(no final message)` when
/// the agent said nothing since the turn's prompt.
pub(super) fn final_message(transcript: &Transcript) -> String {
    transcript
        .entries()
        .iter()
        .rev()
        .take_while(|entry| !starts_turn(entry))
        .find_map(|entry| match entry {
            Entry::Agent(text) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_else(|| NO_FINAL_MESSAGE.to_owned())
}

/// The children the approval card left, `children`, with blank titles derived from the
/// prompts.
///
/// # Errors
///
/// Returns [`AppError::InvalidChildren`] when [`checked_children`] refuses them.
fn titled_children(
    project: &Project,
    children: Vec<ChildChoice>,
) -> Result<Vec<ChildChoice>, AppError> {
    let proposed = children
        .iter()
        .map(|child| ProposedChild {
            repository: child.repository.clone(),
            prompt: child.prompt.clone(),
            title: child.title.clone(),
        })
        .collect();
    let checked = checked_children(project, proposed).map_err(AppError::InvalidChildren)?;
    Ok(checked
        .into_iter()
        .zip(children)
        .map(|(proposed, child)| ChildChoice {
            title: proposed.title,
            ..child
        })
        .collect())
}

/// The repository below the root of `project` with the id `id`.
fn child_repository<'a>(project: &'a Project, id: &RepositoryId) -> Option<&'a Repository> {
    project
        .repositories
        .iter()
        .find(|repository| !repository.is_root && repository.id == *id)
}

/// The steps of a split's git work for `plans`, with how many fetches come first: one fetch
/// for each repository and remote wanted, then each child's step that makes its branch or
/// worktree, in child order.
fn split_steps(plans: &[Option<Planned>]) -> (Vec<GitStep>, usize) {
    let mut steps: Vec<GitStep> = Vec::new();
    let mut makes = Vec::new();
    for planned in plans.iter().flatten() {
        let mut planned_steps = create_steps(planned);
        makes.extend(planned_steps.pop());
        for fetch in planned_steps {
            if !steps.contains(&fetch) {
                steps.push(fetch);
            }
        }
    }
    let fetches = steps.len();
    steps.extend(makes);
    (steps, fetches)
}

/// The proposed `children` as they are recorded, with blank titles derived from the prompts,
/// or the text that refuses them: there is no child, a child names a repository that is not
/// one of `project`'s non-root repositories, or a child's prompt is blank.
fn checked_children(
    project: &Project,
    children: Vec<ProposedChild>,
) -> Result<Vec<ProposedChild>, String> {
    if children.is_empty() {
        return Err(NO_CHILDREN.to_owned());
    }
    let repositories: Vec<_> = project
        .repositories
        .iter()
        .filter(|repository| !repository.is_root)
        .collect();
    (1_usize..)
        .zip(children)
        .map(|(number, mut child)| {
            if !repositories
                .iter()
                .any(|repository| repository.id == child.repository)
            {
                let names: Vec<_> = repositories
                    .iter()
                    .map(|repository| repository.name.as_str())
                    .collect();
                return Err(format!(
                    "unknown repository \"{}\"; the repositories are: {}",
                    child.repository,
                    names.join(", ")
                ));
            }
            if child.prompt.trim().is_empty() {
                return Err(format!("child {number} has an empty prompt"));
            }
            if child.title.trim().is_empty() {
                child.title = title_from_prompt(&child.prompt);
            }
            Ok(child)
        })
        .collect()
}

/// Whether `entry` is the prompt a turn starts with.
fn starts_turn(entry: &Entry) -> bool {
    matches!(
        entry,
        Entry::User(_)
            | Entry::OrchestratorPrompt(_)
            | Entry::PartiturPrompt(_)
            | Entry::ChildResult { .. }
            | Entry::AgentTurn
    )
}

#[cfg(test)]
mod tests;
