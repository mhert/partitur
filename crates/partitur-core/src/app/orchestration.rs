//! How Partitur talks to an orchestrator on its own: the orchestrator's tool calls are
//! answered, a child's finished turn reaches its parent as a prompt, and prompts Partitur
//! generates for a busy session wait for its turn to end.
//!
//! Every child result carries where the child stands in its workflow. A child whose turn ends
//! at a gate or with a question open does not wait for the user while it has a live
//! orchestrator: the orchestrator hears at once, with a `child waits` prompt. The child list
//! and a child's last result name the same state. The orchestrator decides with
//! `send_to_child`: a decision approves the child's gate or asks for changes, and a prompt
//! answers its question; either is recorded as the orchestrator's.
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
    ApprovedChild, ChildOutcome, ChildState, ChildWait, Decision, GateDecision, OrchestratorTool,
    ProposedChild, TitleProposal, TurnOrigin, Update,
};
use crate::git::{GitAnswer, GitError, GitQuery, GitStep};
use crate::ids::{BackendId, GitRequestId, ProposalId, RepositoryId, SessionId};
use crate::orchestrator::{
    PROPOSAL_RECORDED, child_result_prompt, children_started_prompt, orchestrated_repositories,
    rejection_prompt, state_line, state_lines, title_proposal_prompt,
};
use crate::project::{Project, Repository};
use crate::question::QuestionOutcome;
use crate::session::{
    BranchChoice, BranchMode, Scope, Status, TitleSource, Workspace, title_from_prompt,
};
use crate::settings::ProjectSettings;
use crate::tool_call::{PartiturTool, SessionTool, ToolCall, ToolRole};
use crate::transcript::{Entry, Transcript};
use crate::view_model::{ApprovalView, ChildCheck};
use crate::workflow::{self, Closing, Decider, GateCloser};

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

/// The answer to a `propose_title` call from a session that is not a child.
const NOT_A_CHILD: &str = "this session is not a child";

/// The answer to a `rename_session` call from a child.
const CHILD_PROPOSES_TITLES: &str =
    "a child cannot rename itself; propose a title with propose_title";

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
    /// Where each child's title comes from, in child order.
    titles: Vec<TitleSource>,
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
        /// Where the child stands in its workflow, and what it waits for.
        state: ChildState,
    },
    /// A child's title proposal, brought to its orchestrator.
    TitleProposal(TitleProposal),
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
                state,
            } => Update::ChildResult {
                child: child.clone(),
                repository: repository.clone(),
                title: title.clone(),
                origin: *origin,
                outcome: outcome.clone(),
                text: text.clone(),
                state: state.clone(),
            },
            Self::TitleProposal(proposal) => Update::TitleProposed(proposal.clone()),
        }
    }

    /// Whether `newer` takes this prompt's place while both wait: a child's title proposal
    /// replaces the one it made before.
    fn replaced_by(&self, newer: &Self) -> bool {
        matches!(
            (self, newer),
            (Self::TitleProposal(older), Self::TitleProposal(newer)) if older.child == newer.child
        )
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
                state,
            } => child_result_prompt(child, repository, title, *origin, outcome, state, text),
            Self::TitleProposal(proposal) => title_proposal_prompt(proposal),
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
        let recorded = self
            .proposals
            .get(&(session.clone(), proposal.clone()))
            .map_or(&[][..], Vec::as_slice);
        let (children, titles) = titled_children(self.project(&project)?, recorded, children)?;
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
            titles,
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
            titles,
            plans,
        } = approving;
        let mut created = 0;
        let approved = match self.start_each(&session, &children, &titles, &plans, &mut created) {
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

    /// Creates and prompts each of `children` as a child of `session`, titled from `titles`
    /// and working in the workspace its plan in `plans` makes; `created` counts the sessions
    /// created so far. Each child's proposed prompt starts its first turn, so it becomes the
    /// child's first prompt.
    ///
    /// # Errors
    ///
    /// As [`start_children`](Self::start_children).
    fn start_each(
        &mut self,
        session: &SessionId,
        children: &[ChildChoice],
        titles: &[TitleSource],
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
        for ((child, title_source), planned) in children.iter().zip(titles).zip(plans) {
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
                    title_source: *title_source,
                },
                workspace.clone(),
            )?;
            *created += 1;
            let cwd = self.session(&id)?.cwd.clone();
            self.start_generated(&id, &Generated::FromOrchestrator(child.prompt.clone()))?;
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
    /// with `wake`. An orchestrator that waited only for its proposals, not at a gate, becomes
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
            && !self.awaits_decision(&session)
            && self.gate_open(&session).is_none();
        if waited_for_proposals {
            self.set_status(session.clone(), Status::Idle)?;
        }
        if !self.deliver(&session, Generated::Wake(wake))? && !self.in_turn(&session)? {
            self.deliver_next(&session)?;
        }
        Ok(())
    }

    /// Handles a session's tool call: validates it against the core's state, records
    /// what it changes and answers through the call's reply. Never fails on a bad call: the
    /// caller gets the error text.
    ///
    /// A call whose caller stopped waiting, checked when the call is taken up, changes
    /// nothing. A call from a session that does not exist or is not attached is answered with
    /// `unknown session`, one from a session that is not an orchestrator with `this session
    /// is not an orchestrator` when it asks for an orchestrator's tool or names a session to
    /// rename, `rename_session` from a child with the hint to propose a title instead, and
    /// `propose_title` from a session that is not a child with `this session is not a child`;
    /// none of them changes anything. A call whose handling fails is answered with the failure's text before the
    /// failure is returned, so the caller does not wait for an answer that never comes.
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
        let handled = match (self.tool_refusal(&session, &tool), tool) {
            (Some(refusal), _) => Ok(Err(refusal.to_owned())),
            (None, PartiturTool::Workflow(event)) => self.workflow_reported(&session, event),
            (None, PartiturTool::AskUser(question)) => self.ask(&session, question),
            (
                None,
                PartiturTool::Session(SessionTool::Rename {
                    title,
                    session: target,
                }),
            ) => self.rename_by_agent(&session, target.as_ref(), &title),
            (None, PartiturTool::Session(SessionTool::ProposeTitle { title })) => {
                self.propose_title(&session, &title)
            }
            (None, PartiturTool::Orchestrator(tool)) => match tool {
                OrchestratorTool::ProposeChildren(children) => self.propose(&session, children),
                OrchestratorTool::SendToChild {
                    child,
                    prompt,
                    decision,
                } => self.send_to_child(&session, &child, prompt, decision),
                OrchestratorTool::ListChildren => self.list_children(&session).map(Ok),
                OrchestratorTool::CancelChild { child } => self.cancel_child(&session, &child),
                OrchestratorTool::GetChildResult { child } => {
                    self.get_child_result(&session, &child)
                }
            },
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

    /// Why a call of `tool` from `session` is refused before looking at it, or `None` when it
    /// is taken up. Only an orchestrator may call an orchestrator's tool, report on a whole run
    /// or rename another session; every other report about the workflow is taken from every
    /// session. A child never renames itself.
    fn tool_refusal(&self, session: &SessionId, tool: &PartiturTool) -> Option<&'static str> {
        if let Some(refusal) = self.unknown_caller(session) {
            return Some(refusal);
        }
        let orchestrators_only = match tool {
            PartiturTool::Orchestrator(_) => true,
            PartiturTool::Workflow(event) => event.run_level(),
            PartiturTool::AskUser(_) => false,
            PartiturTool::Session(SessionTool::Rename {
                session: target, ..
            }) => {
                if self
                    .session(session)
                    .is_ok_and(|caller| caller.tool_role() == ToolRole::Child)
                {
                    return Some(CHILD_PROPOSES_TITLES);
                }
                target.is_some()
            }
            PartiturTool::Session(SessionTool::ProposeTitle { .. }) => {
                return self
                    .session(session)
                    .is_ok_and(|caller| caller.tool_role() != ToolRole::Child)
                    .then_some(NOT_A_CHILD);
            }
        };
        (orchestrators_only
            && self
                .session(session)
                .is_ok_and(|caller| !caller.orchestrator))
        .then_some(NOT_AN_ORCHESTRATOR)
    }

    /// Why a call from `session` is refused whatever it asks for: the session was deleted, or
    /// does not exist or is not attached.
    fn unknown_caller(&self, session: &SessionId) -> Option<&'static str> {
        if self.deleted.contains(session) {
            Some(DELETED_SESSION)
        } else if self.session(session).is_ok() && self.attached.contains(session) {
            None
        } else {
            Some(UNKNOWN_SESSION)
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

    /// `send_to_child`: prompts the orchestrator's child `child`, decides the gate it waits
    /// at or answers its question, and records in the orchestrator's transcript which child it
    /// prompted with what, and with which decision.
    ///
    /// A blank prompt is refused before the child is looked at, unless it comes with an
    /// approval, and so is a session that does not exist or is not a child of `session`. A
    /// plain prompt to a child with neither a gate nor a question open goes out now or is
    /// queued until the child is free, as [`deliver`](Self::deliver) decides. Every other call
    /// is refused while the child is still in its turn, and otherwise:
    ///
    /// - With a question open (which takes precedence over a gate), the prompt answers it, as
    ///   the user's answer does, and closes the gate too; a decision is refused.
    /// - At a gate, an approval sends the gate's approval text, followed by the prompt after a
    ///   blank line when it is not blank, and closes the gate as approved; a request for changes
    ///   sends the gate's change prefix and the prompt, and closes the gate as a prompt does.
    ///   No decision is refused.
    /// - A decision for a child with no open gate is refused.
    ///
    /// The orchestrator is recorded as the one that decided. A child that takes no prompt
    /// (archived, or its backend gone) refuses a decision or an answer with the reason. A
    /// refused call records nothing.
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
        decision: Option<GateDecision>,
    ) -> Result<Result<String, String>, AppError> {
        if prompt.trim().is_empty() && decision != Some(GateDecision::Approve) {
            return Ok(Err(NO_PROMPT.to_owned()));
        }
        if let Err(refusal) = self.check_child(session, child) {
            return Ok(Err(refusal));
        }
        let question = self.question_open(child);
        let gate = self.gate_open(child);
        let sent = if decision.is_none() && !question && gate.is_none() {
            self.deliver(child, Generated::FromOrchestrator(prompt.clone()))?
        } else {
            if self.in_turn(child)? {
                return Ok(Err(format!("child {child} is still in its turn")));
            }
            let (text, by) = match (question, gate, decision) {
                (true, _, None) => (prompt.clone(), GateCloser::Prompt),
                (true, _, Some(_)) => {
                    return Ok(Err(format!(
                        "child {child} asks a question; answer it with a prompt and no decision"
                    )));
                }
                (false, None, _) => return Ok(Err(format!("child {child} has no open gate"))),
                (false, Some((module, kind)), None) => {
                    let word = workflow::gate_texts(module, &kind).word;
                    return Ok(Err(format!(
                        "child {child} waits at its {word}; send decision \"approve\" or \
                         \"changes\""
                    )));
                }
                (false, Some((module, kind)), Some(GateDecision::Approve)) => {
                    let approval = workflow::gate_texts(module, &kind).approval_text;
                    let text = if prompt.trim().is_empty() {
                        approval.to_owned()
                    } else {
                        format!("{approval}\n\n{prompt}")
                    };
                    (text, GateCloser::Approve)
                }
                (false, Some((module, kind)), Some(GateDecision::Changes)) => {
                    let prefix = workflow::gate_texts(module, &kind).change_prefix;
                    (format!("{prefix}{prompt}"), GateCloser::Prompt)
                }
            };
            if let Err(refusal) = self.check_promptable(child) {
                return Ok(Err(refusal.to_string()));
            }
            self.start_turn(
                child,
                &Update::OrchestratorPrompt(text.clone()),
                text,
                Vec::new(),
                TurnOrigin::Orchestrator,
                Some(Closing {
                    by,
                    decider: Decider::Orchestrator,
                }),
            )?;
            if question {
                self.close_question(
                    child,
                    QuestionOutcome::Answered(prompt.clone()),
                    Decider::Orchestrator,
                )?;
            }
            true
        };
        let prompted = Update::ChildPrompted {
            child: child.clone(),
            prompt,
            decision,
        };
        self.apply_update(session, &prompted)?;
        let answer = if sent {
            format!("prompt sent to child {child}")
        } else {
            format!("prompt queued for child {child}; it goes out when the child is free")
        };
        Ok(Ok(answer))
    }

    /// `cancel_child`: stops the current turn of `child` when it is in a turn or has a prompt
    /// that has not gone out; any other child is reported as not running.
    ///
    /// # Errors
    ///
    /// As [`stop_turn`](Self::stop_turn).
    fn cancel_child(
        &mut self,
        session: &SessionId,
        child: &SessionId,
    ) -> Result<Result<String, String>, AppError> {
        if let Err(refusal) = self.check_child(session, child) {
            return Ok(Err(refusal));
        }
        if self.in_turn(child)? || self.queued.contains_key(child) {
            self.stop_turn(child.clone())?;
            return Ok(Ok(format!(
                "cancel requested for child {child}; its result follows as a child result"
            )));
        }
        let status = status_word(self.session(child)?.status);
        Ok(Ok(format!("child {child} is not running ({status})")))
    }

    /// `get_child_result`: the last final message of `child` with its status and, for a child
    /// with a workflow or a wait, the lines of its [`child_state`](Self::child_state) under
    /// the header; or a note that the child is still working or has not answered yet.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Store`] when the child's transcript cannot be loaded.
    fn get_child_result(
        &mut self,
        session: &SessionId,
        child: &SessionId,
    ) -> Result<Result<String, String>, AppError> {
        if let Err(refusal) = self.check_child(session, child) {
            return Ok(Err(refusal));
        }
        let status = status_word(self.session(child)?.status);
        if self.in_turn(child)? || self.queued.contains_key(child) {
            return Ok(Ok(format!(
                "child {child} is still working ({status}); its result follows as a child result"
            )));
        }
        let text = final_message(self.ensure_transcript(child)?);
        let header = std::iter::once(format!("child {child} · {status}"))
            .chain(state_lines(&self.child_state(child)?))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(Ok(if text != NO_FINAL_MESSAGE {
            format!("{header}\n\n{text}")
        } else if self.session(child)?.status == Status::Failed {
            format!("child {child} failed without a final message")
        } else {
            format!("child {child} has no result yet ({status})")
        }))
    }

    /// Whether `child` is a session of this app whose parent is the orchestrator `session`;
    /// otherwise the refusal text for the tool call.
    pub(super) fn check_child(&self, session: &SessionId, child: &SessionId) -> Result<(), String> {
        let Ok(target) = self.session(child) else {
            return Err(format!("unknown child {child}"));
        };
        if target.parent.as_ref() != Some(session) {
            return Err(format!("{child} is not a child of this session"));
        }
        Ok(())
    }

    /// `list_children`: one line per direct child of the orchestrator `session`, in session-list
    /// order, with its title, repository, session id and status, then the [`state_line`] of a
    /// child with a workflow or a wait, and `set by you` after a title the user typed; a note
    /// when it has none.
    ///
    /// # Errors
    ///
    /// As [`child_state`](Self::child_state).
    fn list_children(&mut self, session: &SessionId) -> Result<String, AppError> {
        let children: Vec<(SessionId, String, &'static str)> = self
            .sessions
            .iter()
            .filter_map(|child| match (&child.parent, &child.scope) {
                (Some(parent), Scope::Repository(repository)) if parent == session => {
                    let by_user = if child.title_source == TitleSource::Given {
                        " · set by you"
                    } else {
                        ""
                    };
                    let line = format!(
                        "- {} · {repository} · session {} · {}",
                        child.title,
                        child.id,
                        status_word(child.status)
                    );
                    Some((child.id.clone(), line, by_user))
                }
                _ => None,
            })
            .collect();
        if children.is_empty() {
            return Ok("no children yet".to_owned());
        }
        let mut lines = Vec::new();
        for (child, line, by_user) in children {
            let state = state_line(&self.child_state(&child)?)
                .map_or_else(String::new, |state| format!(" · {state}"));
            lines.push(format!("{line}{state}{by_user}"));
        }
        Ok(format!("children:\n{}", lines.join("\n")))
    }

    /// Where `child` stands in its workflow: the step of its tree cell, `None` without a
    /// workflow, and what it waits for: its open question, taken from its transcript, which is
    /// loaded for it, or else its open gate, named by the gate's word; `None` when neither is
    /// open.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists and
    /// [`AppError::Store`] when the child's transcript cannot be loaded.
    pub(super) fn child_state(&mut self, child: &SessionId) -> Result<ChildState, AppError> {
        let session = self.session(child)?;
        let step = session
            .workflow
            .as_ref()
            .map(|status| workflow::step_cell(status).label);
        let question = match session.question.clone() {
            Some(open) => self
                .ensure_transcript(child)?
                .open_question()
                .filter(|(id, _)| **id == open)
                .map(|(_, question)| ChildWait::Question(question.clone())),
            None => None,
        };
        let waits = question.or_else(|| {
            self.gate_open(child).map(|(module, kind)| ChildWait::Gate {
                word: workflow::gate_texts(module, &kind).word.to_owned(),
            })
        });
        Ok(ChildState { step, waits })
    }

    /// Whether `child` has an orchestrator to decide its gates and answer its questions: its
    /// parent exists, is an orchestrator and is not archived.
    pub(super) fn delegates(&self, child: &SessionId) -> bool {
        self.session(child)
            .ok()
            .and_then(|session| session.parent.as_ref())
            .and_then(|parent| self.session(parent).ok())
            .is_some_and(|parent| parent.orchestrator && parent.archived.is_none())
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
            let queue = self.deferred.entry(session.clone()).or_default();
            let waiting = queue
                .iter_mut()
                .find(|queued| queued.replaced_by(&generated));
            match waiting {
                Some(queued) => *queued = generated,
                None => queue.push_back(generated),
            }
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
        let generated = self.refreshed(generated);
        self.start_generated(session, &generated)
    }

    /// `generated` as it reads now: a title proposal takes its child's current title, and
    /// whether the user typed it, in place of the ones it was made with. A proposal whose
    /// child is gone, and every other prompt, stays as it is.
    fn refreshed(&self, generated: Generated) -> Generated {
        match generated {
            Generated::TitleProposal(mut proposal) => {
                if let Ok(child) = self.session(&proposal.child) {
                    proposal.title.clone_from(&child.title);
                    proposal.by_user = child.title_source == TitleSource::Given;
                }
                Generated::TitleProposal(proposal)
            }
            other => other,
        }
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
    /// The result carries the child's [`child_state`](Self::child_state). A turn that ended
    /// with the child waiting at a gate, or with a question of the child's open, reaches a
    /// parent that [decides for it](Self::delegates) as a `child waits` prompt, so the
    /// orchestrator decides the gate or answers the question. Without such a parent it reports
    /// nothing: the user decides the gate or answers the question, and the next turn that ends
    /// with neither open reports its own final message. A failed turn is reported as a child
    /// result whatever gate or question is open, with its step but no wait.
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
        let ended = matches!(outcome, ChildOutcome::Ended(_));
        if ended
            && (self.gate_open(child).is_some() || self.question_open(child))
            && !self.delegates(child)
        {
            return Ok(());
        }
        let (parent, repository, title) =
            (parent.clone(), repository.clone(), session.title.clone());
        let mut state = self.child_state(child)?;
        if !ended {
            state.waits = None;
        }
        let result = Generated::ChildResult {
            child: child.clone(),
            repository,
            title,
            origin,
            outcome,
            text: final_message(self.ensure_transcript(child)?),
            state,
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
    pub(super) fn ensure_transcript(&mut self, id: &SessionId) -> Result<&Transcript, AppError> {
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

    /// Whether the session takes no generated prompt for now: its project is closed, the
    /// session is archived, or it waits at a gate of its own. Its generated prompts wait until
    /// it is reopened or restored, or the user decided the gate: only the user decides a gate;
    /// a child result would otherwise drive an orchestrator past it.
    fn parked(&self, session: &SessionId) -> Result<bool, AppError> {
        let project = &self.session(session)?.project;
        Ok(self.project(project)?.closed
            || self.is_archived(session)?
            || self.gate_open(session).is_some())
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
            None,
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
/// prompts, and where each title comes from. A title comes from the prompt, and an agent's
/// title may replace it, when the proposal derived it and the card left it as it was, or the
/// card left it blank. A title the orchestrator gave that the card left as it was is
/// [chosen](TitleSource::Chosen); every title the user changed is [given](TitleSource::Given).
///
/// `proposed` are the children as the proposal recorded them. Each card row is matched to
/// the recorded child at the same position, so once the card removes or adds a child, a row
/// may be compared with another child. A mismatch costs only the title's source: a derived
/// title left on a row that moved reads as given, and an agent's title no longer replaces it;
/// a row whose title happens to equal the derived title recorded at its position reads as
/// derived, and an agent's title may replace it.
///
/// # Errors
///
/// Returns [`AppError::InvalidChildren`] when [`checked_children`] refuses them.
fn titled_children(
    project: &Project,
    proposed: &[ProposedChild],
    children: Vec<ChildChoice>,
) -> Result<(Vec<ChildChoice>, Vec<TitleSource>), AppError> {
    let titles = children
        .iter()
        .enumerate()
        .map(|(index, child)| {
            let left = proposed
                .get(index)
                .filter(|recorded| recorded.title == child.title)
                .map(|recorded| recorded.title_source);
            match left {
                _ if child.title.trim().is_empty() => TitleSource::Derived,
                Some(TitleSource::Derived) => TitleSource::Derived,
                Some(_) => TitleSource::Chosen,
                None => TitleSource::Given,
            }
        })
        .collect();
    let to_check = children
        .iter()
        .map(|child| ProposedChild {
            repository: child.repository.clone(),
            prompt: child.prompt.clone(),
            title: child.title.clone(),
            title_source: TitleSource::Given,
        })
        .collect();
    let checked = checked_children(project, to_check).map_err(AppError::InvalidChildren)?;
    let children = checked
        .into_iter()
        .zip(children)
        .map(|(checked, child)| ChildChoice {
            title: checked.title,
            ..child
        })
        .collect();
    Ok((children, titles))
}

/// The repository with the id `id` among those an orchestrator in `project` works on.
fn child_repository<'a>(project: &'a Project, id: &RepositoryId) -> Option<&'a Repository> {
    orchestrated_repositories(project)
        .into_iter()
        .find(|repository| repository.id == *id)
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
/// one an orchestrator in `project` works on, or a child's prompt is blank.
fn checked_children(
    project: &Project,
    children: Vec<ProposedChild>,
) -> Result<Vec<ProposedChild>, String> {
    if children.is_empty() {
        return Err(NO_CHILDREN.to_owned());
    }
    let repositories = orchestrated_repositories(project);
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
            child.title_source = if child.title.trim().is_empty() {
                child.title = title_from_prompt(&child.prompt);
                TitleSource::Derived
            } else {
                TitleSource::Given
            };
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
            | Entry::TitleProposal(_)
            | Entry::AgentTurn
    )
}

#[cfg(test)]
mod tests;
