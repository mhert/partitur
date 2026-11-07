//! How Partitur talks to an orchestrator on its own: the orchestrator's tool calls are
//! answered, a child's finished turn reaches its parent as a prompt, and prompts Partitur
//! generates for a busy session wait for its turn to end.

use std::collections::VecDeque;

use super::{App, AppError};
use crate::backend::{
    ApprovedChild, ChildOutcome, Decision, OrchestratorTool, ProposedChild, TurnOrigin, Update,
};
use crate::ids::{ProposalId, RepositoryId, SessionId, ToolRequestId};
use crate::orchestrator::{
    PROPOSAL_RECORDED, child_result_prompt, children_started_prompt, rejection_prompt,
};
use crate::project::Project;
use crate::session::{Scope, Status, title_from_prompt};
use crate::transcript::{Entry, Transcript};

/// What a child reports when its turn ended without an answer from the agent.
const NO_FINAL_MESSAGE: &str = "(no final message)";

/// The answer to a tool call from a session that does not exist or is not attached.
const UNKNOWN_SESSION: &str = "unknown session";

/// The answer to a tool call from a session that is not an orchestrator.
const NOT_AN_ORCHESTRATOR: &str = "this session is not an orchestrator";

/// The answer to a `propose_children` call without children.
const NO_CHILDREN: &str = "propose_children needs at least one child";

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
    /// Approves the undecided proposal `proposal` of the orchestrator `session` with
    /// `children` as the approval card leaves them, and returns the started children's
    /// sessions in order.
    ///
    /// Each child is a new session in its repository, a child of `session`, titled as chosen
    /// (a blank title is derived from the prompt) and granting permissions without asking when
    /// chosen; it is not selected. It is [`Running`](Status::Running) at once, and its prompt
    /// goes out, after its repository's instruction chain, as soon as its backend session is
    /// created. The decision goes into the orchestrator's
    /// transcript, and the orchestrator is woken with the started children and their session
    /// ids: at once when it is idle or was waiting only for its proposals, else once its turn
    /// ends, and always after child results that arrived before the decision.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::UnknownProposal`] when the session's loaded transcript has no undecided
    /// proposal `proposal`, [`AppError::InvalidChildren`] when `children` is empty, names a
    /// repository that is not one of the project's or has a blank prompt (nothing changes in
    /// these cases), [`AppError::Backend`] when the backend cannot queue a command and
    /// [`AppError::Store`] when the change cannot be saved.
    pub fn approve_proposal(
        &mut self,
        session: SessionId,
        proposal: ProposalId,
        children: Vec<ChildChoice>,
    ) -> Result<Vec<SessionId>, AppError> {
        self.check_undecided(&session, &proposal)?;
        let project = self.session(&session)?.project.clone();
        let auto_allow: Vec<bool> = children.iter().map(|child| child.auto_allow).collect();
        let proposed = children
            .into_iter()
            .map(|child| ProposedChild {
                repository: child.repository,
                prompt: child.prompt,
                title: child.title,
            })
            .collect();
        let checked = checked_children(self.project(&project)?, proposed)
            .map_err(AppError::InvalidChildren)?;
        let mut approved = Vec::new();
        for (child, auto_allow) in checked.into_iter().zip(auto_allow) {
            let id = self.create_session(
                project.clone(),
                Scope::Repository(child.repository.clone()),
                Some(session.clone()),
                child.title.clone(),
                auto_allow,
            )?;
            let record = Update::OrchestratorPrompt(child.prompt.clone());
            let text = child.prompt.clone();
            self.start_turn(&id, &record, text, Vec::new(), TurnOrigin::Orchestrator)?;
            approved.push(ApprovedChild {
                session: id,
                repository: child.repository,
                prompt: child.prompt,
                title: child.title,
                auto_allow,
            });
        }
        let started = approved.iter().map(|child| child.session.clone()).collect();
        let wake = children_started_prompt(&approved);
        self.decide(session, proposal, Decision::Approved(approved), wake)?;
        Ok(started)
    }

    /// Rejects the undecided proposal `proposal` of the orchestrator `session` with the
    /// user's `note` (a blank one counts as none). The rejection goes into the orchestrator's
    /// transcript and wakes it with the note, as
    /// [`approve_proposal`](Self::approve_proposal) wakes it with the started children.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::UnknownProposal`] when the session's loaded transcript has no undecided
    /// proposal `proposal` (nothing changes in these cases), [`AppError::Backend`] when the
    /// backend cannot queue the prompt and [`AppError::Store`] when the change cannot be
    /// saved.
    pub fn reject_proposal(
        &mut self,
        session: SessionId,
        proposal: ProposalId,
        note: Option<String>,
    ) -> Result<(), AppError> {
        self.check_undecided(&session, &proposal)?;
        let note = note.filter(|note| !note.trim().is_empty());
        let wake = rejection_prompt(note.as_deref());
        self.decide(session, proposal, Decision::Rejected { note }, wake)
    }

    /// Fails unless the session's loaded transcript has the undecided proposal `proposal`.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists and
    /// [`AppError::UnknownProposal`] when the proposal is not undecided there.
    fn check_undecided(&self, session: &SessionId, proposal: &ProposalId) -> Result<(), AppError> {
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

    /// `BackendEvent::ToolCalled`: validates the call against the core's state, records what
    /// it changes and answers the backend. Never fails on a bad call: the orchestrator gets
    /// the error text.
    ///
    /// A call from a session that does not exist or is not attached is answered with
    /// `unknown session`, one from a session that is not an orchestrator with `this session
    /// is not an orchestrator`; neither changes anything. A call whose handling fails is
    /// answered with the failure's text before the failure is returned, so the orchestrator
    /// does not wait for an answer that never comes.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Backend`] when the answer or a prompt cannot be queued and
    /// [`AppError::Store`] when the change cannot be saved.
    pub(super) fn tool_called(
        &mut self,
        call: ToolRequestId,
        session: &SessionId,
        tool: OrchestratorTool,
    ) -> Result<(), AppError> {
        let handled = match (self.tool_refusal(session), tool) {
            (Some(refusal), _) => Ok(Err(refusal.to_owned())),
            (None, OrchestratorTool::ProposeChildren(children)) => self.propose(session, children),
            (None, OrchestratorTool::SendToChild { child, prompt }) => {
                self.send_to_child(session, &child, prompt)
            }
        };
        match handled {
            Ok(result) => {
                self.backend.answer_tool(call, result)?;
                Ok(())
            }
            Err(error) => {
                // The failure that is returned matters more than whether its answer queued.
                drop(self.backend.answer_tool(call, Err(error.to_string())));
                Err(error)
            }
        }
    }

    /// Why a tool call from `session` is refused before looking at it, or `None` when the
    /// session is an attached orchestrator.
    fn tool_refusal(&self, session: &SessionId) -> Option<&'static str> {
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

    /// `send_to_child`: prompts the orchestrator's child `child` now, or once its turn ends
    /// when it is busy, as [`deliver`](Self::deliver) decides. A session that does not exist
    /// or is not a child of `session` is refused.
    ///
    /// # Errors
    ///
    /// As [`deliver`](Self::deliver).
    fn send_to_child(
        &mut self,
        session: &SessionId,
        child: &SessionId,
        prompt: String,
    ) -> Result<Result<String, String>, AppError> {
        let Ok(target) = self.session(child) else {
            return Ok(Err(format!("unknown child {child}")));
        };
        if target.parent.as_ref() != Some(session) {
            return Ok(Err(format!("{child} is not a child of this session")));
        }
        let answer = if self.deliver(child, Generated::FromOrchestrator(prompt))? {
            format!("prompt sent to child {child}")
        } else {
            format!("child {child} is mid-turn; the prompt goes out when its turn ends")
        };
        Ok(Ok(answer))
    }

    /// Whether the session has a proposal the user has not decided yet.
    pub(super) fn awaits_decision(&self, session: &SessionId) -> bool {
        self.proposals
            .keys()
            .any(|(proposer, _)| proposer == session)
    }

    /// Starts a turn with `generated` when the session is neither running a turn nor waiting
    /// for the user, its project is open and no prompt is deferred for it yet, else queues it
    /// behind the prompts already deferred, so generated prompts go out in arrival order;
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

    /// Sends the oldest prompt deferred for `session`, if any, unless the session's project
    /// is closed.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::Backend`] when the backend cannot queue the prompt and [`AppError::Store`]
    /// when the change cannot be saved.
    pub(super) fn deliver_next(&mut self, session: &SessionId) -> Result<(), AppError> {
        if self.project_closed(session)? {
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
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when `child` or its parent does not exist,
    /// [`AppError::Store`] when its transcript cannot be loaded or the change cannot be saved
    /// and [`AppError::Backend`] when the backend cannot queue the prompt.
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
        self.session(id)?;
        if !self.transcripts.contains_key(id) {
            let updates = self.store.load_updates(id.clone())?;
            self.transcripts
                .insert(id.clone(), Transcript::from_updates(&updates));
        }
        self.transcripts
            .get(id)
            .ok_or_else(|| AppError::UnknownSession(id.clone()))
    }

    /// Whether a generated prompt may start a turn of the session now: it is neither in a
    /// turn nor in a closed project, and no older generated prompt waits for it.
    fn may_start_generated(&self, session: &SessionId) -> Result<bool, AppError> {
        let nothing_deferred = self.deferred.get(session).is_none_or(VecDeque::is_empty);
        Ok(!self.in_turn(session)? && !self.project_closed(session)? && nothing_deferred)
    }

    /// Whether the session's project is closed.
    fn project_closed(&self, session: &SessionId) -> Result<bool, AppError> {
        let project = &self.session(session)?.project;
        Ok(self.project(project)?.closed)
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
    )
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
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;
    use std::collections::VecDeque;

    use crate::backend::{
        ApprovedChild, BackendEvent, ChildOutcome, Decision, OrchestratorTool, PermissionKind,
        PermissionOption, PromptContent, ProposedChild, StopReason, ToolKind, ToolStatus,
        TurnOrigin, Update,
    };
    use crate::ids::{
        BackendSessionId, PermissionId, PermissionOptionId, ProjectId, RepositoryId, RequestId,
        SessionId, ToolCallId, ToolRequestId,
    };
    use crate::instructions::{inject, instruction_chain};
    use crate::orchestrator::{PROPOSAL_RECORDED, child_result_prompt, children_started_prompt};
    use crate::session::{Scope, Status};
    use crate::store::Store as _;
    use crate::testing::{Call, MemoryStore, RecordingBackend, single_claude};
    use crate::transcript::Entry;

    struct Fixture {
        app: App,
        backend: RecordingBackend,
        store: MemoryStore,
    }

    fn fixture_with(store: MemoryStore) -> Fixture {
        let (backends, default) = single_claude();
        let backend = RecordingBackend::new();
        let app = App::load(
            Box::new(backend.clone()),
            Box::new(store.clone()),
            backends,
            default,
        )
        .unwrap();
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

    fn repository(name: &str) -> RepositoryId {
        RepositoryId::try_from(name).unwrap()
    }

    /// Creates a session in `scope` and attaches it, as though its instructions were already
    /// sent, so its prompts reach the backend as given.
    fn attached_session(fixture: &mut Fixture, project: &ProjectId, scope: Scope) -> SessionId {
        let session = fixture.app.new_session(project.clone(), scope).unwrap();
        let calls = fixture.backend.calls();
        let (request, _, _) = calls.last().unwrap().as_new_session().unwrap();
        fixture
            .app
            .apply_event(BackendEvent::SessionCreated {
                request,
                session: session.clone(),
                backend_session: BackendSessionId::new(format!("b-{session}")),
                config: Vec::new(),
            })
            .unwrap();
        fixture
            .app
            .change_session(session.clone(), |attached| {
                attached.instructions_injected = true;
            })
            .unwrap();
        session
    }

    /// An attached child of `parent` working in `repository`, titled `title`.
    fn attached_child(
        fixture: &mut Fixture,
        project: &ProjectId,
        parent: &SessionId,
        name: &str,
        title: &str,
    ) -> SessionId {
        let child = attached_session(fixture, project, Scope::Repository(repository(name)));
        fixture
            .app
            .change_session(child.clone(), |spawned| {
                spawned.parent = Some(parent.clone());
                spawned.title = title.into();
            })
            .unwrap();
        child
    }

    /// An open two-repository project with an attached, idle orchestrator and an attached
    /// child of it in `api` titled "API endpoint".
    struct Family {
        dir: TempDir,
        fixture: Fixture,
        project: ProjectId,
        parent: SessionId,
        child: SessionId,
    }

    fn family() -> Family {
        let dir = project_dir(&["api", "web"]);
        let mut fixture = fixture_with(MemoryStore::new());
        let project = fixture.app.open_project(dir.path()).unwrap();
        let parent = attached_session(&mut fixture, &project, Scope::Project);
        assert!(fixture.app.session(&parent).unwrap().orchestrator);
        let child = attached_child(&mut fixture, &project, &parent, "api", "API endpoint");
        Family {
            dir,
            fixture,
            project,
            parent,
            child,
        }
    }

    fn send(fixture: &mut Fixture, session: &SessionId, update: Update) {
        fixture
            .app
            .apply_event(BackendEvent::Update {
                session: session.clone(),
                update,
            })
            .unwrap();
    }

    /// Starts a turn of `child` as its orchestrator would.
    fn orchestrate(fixture: &mut Fixture, child: &SessionId) {
        fixture
            .app
            .deliver(
                child,
                Generated::FromOrchestrator("Add the endpoint".into()),
            )
            .unwrap();
    }

    /// A child of `parent` in `api` titled "API endpoint" whose backend session is still
    /// being created, with the id of the create request.
    fn unattached_child(
        fixture: &mut Fixture,
        project: &ProjectId,
        parent: &SessionId,
    ) -> (SessionId, RequestId) {
        let child = fixture
            .app
            .new_session(project.clone(), Scope::Repository(repository("api")))
            .unwrap();
        let calls = fixture.backend.calls();
        let (request, _, _) = calls.last().unwrap().as_new_session().unwrap();
        fixture
            .app
            .change_session(child.clone(), |spawned| {
                spawned.parent = Some(parent.clone());
                spawned.title = "API endpoint".into();
            })
            .unwrap();
        (child, request)
    }

    /// Lets `child` finish a turn its orchestrator started with the answer `answer`.
    fn finish(fixture: &mut Fixture, child: &SessionId, answer: &str) {
        orchestrate(fixture, child);
        send(fixture, child, Update::AgentText(answer.into()));
        send(fixture, child, Update::TurnEnded(StopReason::EndTurn));
    }

    fn deferred(app: &App, session: &SessionId) -> usize {
        app.deferred.get(session).map_or(0, VecDeque::len)
    }

    fn status(app: &App, session: &SessionId) -> Status {
        app.session(session).unwrap().status
    }

    /// The texts of the prompts `session` was sent, oldest first.
    fn prompts(backend: &RecordingBackend, session: &SessionId) -> Vec<String> {
        backend
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Prompt {
                    session: prompted,
                    content,
                } if &prompted == session => Some(
                    content
                        .into_iter()
                        .filter_map(|part| match part {
                            PromptContent::Text(text) => Some(text),
                            _ => None,
                        })
                        .collect(),
                ),
                _ => None,
            })
            .collect()
    }

    fn last_prompt(backend: &RecordingBackend, session: &SessionId) -> String {
        prompts(backend, session).pop().expect("no prompt was sent")
    }

    fn result_text(
        child: &SessionId,
        origin: TurnOrigin,
        outcome: &ChildOutcome,
        text: &str,
    ) -> String {
        child_result_prompt(
            child,
            &repository("api"),
            "API endpoint",
            origin,
            outcome,
            text,
        )
    }

    fn proposed(name: &str, prompt: &str, title: &str) -> ProposedChild {
        ProposedChild {
            repository: repository(name),
            prompt: prompt.into(),
            title: title.into(),
        }
    }

    /// Lets `session` call `tool` and returns the answer the backend was given.
    fn call_tool(
        fixture: &mut Fixture,
        session: &SessionId,
        tool: OrchestratorTool,
    ) -> Result<String, String> {
        fixture
            .app
            .apply_event(BackendEvent::ToolCalled {
                call: ToolRequestId(7),
                session: session.clone(),
                tool,
            })
            .unwrap();
        let calls = fixture.backend.calls();
        assert!(matches!(
            calls.last(),
            Some(Call::AnswerTool {
                call: ToolRequestId(7),
                ..
            })
        ));
        let mut answers: Vec<_> = calls
            .into_iter()
            .filter_map(|call| match call {
                Call::AnswerTool { result, .. } => Some(result),
                _ => None,
            })
            .collect();
        answers.pop().unwrap()
    }

    fn call_propose(
        fixture: &mut Fixture,
        session: &SessionId,
        children: Vec<ProposedChild>,
    ) -> Result<String, String> {
        call_tool(
            fixture,
            session,
            OrchestratorTool::ProposeChildren(children),
        )
    }

    fn call_send_to_child(
        fixture: &mut Fixture,
        session: &SessionId,
        child: &SessionId,
        prompt: &str,
    ) -> Result<String, String> {
        call_tool(
            fixture,
            session,
            OrchestratorTool::SendToChild {
                child: child.clone(),
                prompt: prompt.into(),
            },
        )
    }

    /// The id of the session's only undecided proposal.
    fn proposal_id(app: &App, session: &SessionId) -> ProposalId {
        let ids: Vec<_> = app
            .proposals
            .keys()
            .filter(|(proposer, _)| proposer == session)
            .map(|(_, id)| id.clone())
            .collect();
        assert_eq!(ids.len(), 1);
        ids[0].clone()
    }

    #[test]
    fn generated_prompts_are_recorded_by_their_kind_and_sent_as_written() {
        let wake = Generated::Wake("children started".into());
        let prompt = Generated::FromOrchestrator("Add the endpoint".into());

        assert_eq!(
            wake.record(),
            Update::PartiturPrompt("children started".into())
        );
        assert_eq!(wake.text(), "children started");
        assert_eq!(
            prompt.record(),
            Update::OrchestratorPrompt("Add the endpoint".into())
        );
        assert_eq!(prompt.text(), "Add the endpoint");
    }

    #[test]
    fn the_final_message_is_the_last_answer_of_the_latest_turn() {
        let answered = Transcript::from_updates(&[
            Update::UserText("go".into()),
            Update::AgentText("first".into()),
            Update::Notice("note".into()),
            Update::AgentText("last".into()),
            Update::TurnEnded(StopReason::EndTurn),
        ]);
        let silent = Transcript::from_updates(&[
            Update::UserText("go".into()),
            Update::AgentText("earlier turn".into()),
            Update::TurnEnded(StopReason::EndTurn),
            Update::OrchestratorPrompt("again".into()),
            Update::TurnEnded(StopReason::EndTurn),
        ]);

        assert_eq!(final_message(&answered), "last");
        assert_eq!(final_message(&silent), "(no final message)");
    }

    #[test]
    fn a_child_turn_end_prompts_its_idle_parent_with_the_result() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        orchestrate(&mut fixture, &child);

        send(&mut fixture, &child, Update::AgentText("half".into()));
        send(&mut fixture, &child, Update::AgentText(" done".into()));
        send(&mut fixture, &child, Update::TurnEnded(StopReason::EndTurn));

        let outcome = ChildOutcome::Ended(StopReason::EndTurn);
        assert_eq!(
            fixture.backend.calls().last(),
            Some(&Call::Prompt {
                session: parent.clone(),
                content: vec![PromptContent::Text(result_text(
                    &child,
                    TurnOrigin::Orchestrator,
                    &outcome,
                    "half done"
                ))],
            })
        );
        assert_eq!(
            fixture.app.transcripts[&parent].entries().last(),
            Some(&Entry::ChildResult {
                child: child.clone(),
                repository: repository("api"),
                title: "API endpoint".into(),
                origin: TurnOrigin::Orchestrator,
                outcome,
                text: "half done".into(),
            })
        );
        assert_eq!(status(&fixture.app, &parent), Status::Running);
        assert_eq!(status(&fixture.app, &child), Status::Done);
    }

    #[test]
    fn a_user_started_child_turn_is_marked_as_such() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        fixture.app.select_session(child.clone()).unwrap();
        fixture
            .app
            .submit_prompt("Also add a test", Vec::new())
            .unwrap();

        send(&mut fixture, &child, Update::AgentText("added".into()));
        send(&mut fixture, &child, Update::TurnEnded(StopReason::EndTurn));

        let text = last_prompt(&fixture.backend, &parent);
        assert!(text.contains("(turn started by the user)"), "{text}");
        assert_eq!(
            text,
            result_text(
                &child,
                TurnOrigin::User,
                &ChildOutcome::Ended(StopReason::EndTurn),
                "added"
            )
        );
    }

    #[test]
    fn a_failed_child_turn_is_marked_failed_with_the_error() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        orchestrate(&mut fixture, &child);

        send(&mut fixture, &child, Update::Failed("boom".into()));

        let text = last_prompt(&fixture.backend, &parent);
        assert!(text.contains("(failed: boom)"), "{text}");
        assert_eq!(status(&fixture.app, &child), Status::Failed);
    }

    #[test]
    fn a_cancelled_child_turn_is_marked_with_its_stop_reason() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        orchestrate(&mut fixture, &child);

        send(
            &mut fixture,
            &child,
            Update::TurnEnded(StopReason::Cancelled),
        );

        let text = last_prompt(&fixture.backend, &parent);
        assert!(text.contains("(turn ended: cancelled)"), "{text}");
    }

    #[test]
    fn a_child_blocked_on_a_permission_produces_no_result() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        orchestrate(&mut fixture, &child);

        send(
            &mut fixture,
            &child,
            Update::PermissionRequested {
                id: PermissionId(1),
                tool_call_id: ToolCallId::new("t-1"),
                title: "Run cargo test".into(),
                kind: ToolKind::Execute,
                options: vec![PermissionOption {
                    id: PermissionOptionId::new("allow"),
                    name: "Allow".into(),
                    kind: PermissionKind::AllowOnce,
                }],
                name: None,
            },
        );

        assert!(prompts(&fixture.backend, &parent).is_empty());
        assert_eq!(status(&fixture.app, &child), Status::WaitingForYou);
        assert_eq!(status(&fixture.app, &parent), Status::Idle);
    }

    #[test]
    fn results_queue_while_the_parent_is_mid_turn_and_go_out_one_per_turn() {
        let Family {
            mut fixture,
            project,
            parent,
            child,
            ..
        } = family();
        let web = attached_child(&mut fixture, &project, &parent, "web", "Web page");
        fixture.app.select_session(parent.clone()).unwrap();
        fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
        for (session, answer) in [(&child, "api done"), (&web, "web done")] {
            orchestrate(&mut fixture, session);
            send(&mut fixture, session, Update::AgentText(answer.into()));
            send(
                &mut fixture,
                session,
                Update::TurnEnded(StopReason::EndTurn),
            );
        }

        assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);

        let ended = ChildOutcome::Ended(StopReason::EndTurn);
        let web_result = child_result_prompt(
            &web,
            &repository("web"),
            "Web page",
            TurnOrigin::Orchestrator,
            &ended,
            "web done",
        );
        send(
            &mut fixture,
            &parent,
            Update::TurnEnded(StopReason::EndTurn),
        );
        assert_eq!(
            last_prompt(&fixture.backend, &parent),
            result_text(&child, TurnOrigin::Orchestrator, &ended, "api done")
        );
        assert_eq!(status(&fixture.app, &parent), Status::Running);

        send(
            &mut fixture,
            &parent,
            Update::TurnEnded(StopReason::EndTurn),
        );
        assert_eq!(last_prompt(&fixture.backend, &parent), web_result);
        assert_eq!(status(&fixture.app, &parent), Status::Running);

        send(
            &mut fixture,
            &parent,
            Update::TurnEnded(StopReason::EndTurn),
        );
        assert_eq!(prompts(&fixture.backend, &parent).len(), 3);
        assert_eq!(status(&fixture.app, &parent), Status::Done);
    }

    #[test]
    fn a_child_result_for_a_detached_parent_waits_for_the_reattach() {
        let Family {
            dir: _dir,
            fixture,
            parent,
            child,
            ..
        } = family();
        let mut restarted = fixture_with(fixture.store.clone());
        drop(fixture);
        restarted.app.select_session(child.clone()).unwrap();
        restarted
            .app
            .submit_prompt("Add the endpoint", Vec::new())
            .unwrap();
        let Some(Call::LoadSession { request, .. }) = restarted.backend.calls().last().cloned()
        else {
            panic!("the child was not loaded");
        };
        restarted
            .app
            .apply_event(BackendEvent::SessionLoaded {
                request,
                session: child.clone(),
                config: Vec::new(),
            })
            .unwrap();
        send(&mut restarted, &child, Update::AgentText("done".into()));

        send(
            &mut restarted,
            &child,
            Update::TurnEnded(StopReason::EndTurn),
        );

        let Some(Call::LoadSession {
            request,
            session,
            orchestrator: true,
            ..
        }) = restarted.backend.calls().last().cloned()
        else {
            panic!("the parent was not loaded as an orchestrator");
        };
        assert_eq!(session, parent);
        assert!(prompts(&restarted.backend, &parent).is_empty());

        restarted
            .app
            .apply_event(BackendEvent::SessionLoaded {
                request,
                session: parent.clone(),
                config: Vec::new(),
            })
            .unwrap();

        assert_eq!(
            prompts(&restarted.backend, &parent),
            [result_text(
                &child,
                TurnOrigin::User,
                &ChildOutcome::Ended(StopReason::EndTurn),
                "done"
            )]
        );
        assert_eq!(status(&restarted.app, &parent), Status::Running);
    }

    #[test]
    fn a_child_without_a_final_message_reports_so() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        orchestrate(&mut fixture, &child);
        send(
            &mut fixture,
            &child,
            Update::ToolCall {
                id: ToolCallId::new("t-1"),
                title: "Read main.rs".into(),
                kind: ToolKind::Read,
                status: ToolStatus::Completed,
                content: Vec::new(),
                locations: Vec::new(),
            },
        );

        send(&mut fixture, &child, Update::TurnEnded(StopReason::EndTurn));

        assert_eq!(
            last_prompt(&fixture.backend, &parent),
            result_text(
                &child,
                TurnOrigin::Orchestrator,
                &ChildOutcome::Ended(StopReason::EndTurn),
                "(no final message)"
            )
        );
    }

    #[test]
    fn a_session_without_a_parent_reports_to_nobody() {
        let Family {
            mut fixture,
            project,
            ..
        } = family();
        let lone = attached_session(&mut fixture, &project, Scope::Repository(repository("web")));
        fixture
            .app
            .submit_prompt("Fix the build", Vec::new())
            .unwrap();
        send(&mut fixture, &lone, Update::AgentText("fixed".into()));
        let calls = fixture.backend.calls();

        send(&mut fixture, &lone, Update::TurnEnded(StopReason::EndTurn));

        assert_eq!(fixture.backend.calls(), calls);
        assert_eq!(status(&fixture.app, &lone), Status::Done);
    }

    #[test]
    fn a_child_whose_transcript_is_not_loaded_is_read_from_the_store() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        orchestrate(&mut fixture, &child);
        send(
            &mut fixture,
            &child,
            Update::AgentText("from the store".into()),
        );
        fixture.app.transcripts.remove(&child);
        assert!(
            !fixture
                .store
                .load_updates(child.clone())
                .unwrap()
                .is_empty()
        );

        send(&mut fixture, &child, Update::TurnEnded(StopReason::EndTurn));

        assert_eq!(
            last_prompt(&fixture.backend, &parent),
            result_text(
                &child,
                TurnOrigin::Orchestrator,
                &ChildOutcome::Ended(StopReason::EndTurn),
                "from the store"
            )
        );
    }

    #[test]
    fn a_closed_project_keeps_results_without_starting_turns() {
        let Family {
            dir,
            mut fixture,
            project,
            parent,
            child,
        } = family();
        orchestrate(&mut fixture, &child);
        send(&mut fixture, &child, Update::AgentText("half".into()));
        fixture.app.close_project(project).unwrap();

        send(
            &mut fixture,
            &child,
            Update::TurnEnded(StopReason::Cancelled),
        );

        assert!(prompts(&fixture.backend, &parent).is_empty());
        assert_eq!(deferred(&fixture.app, &parent), 1);
        assert_eq!(status(&fixture.app, &parent), Status::Idle);

        fixture.app.open_project(dir.path()).unwrap();
        fixture.app.select_session(parent.clone()).unwrap();
        fixture.app.submit_prompt("Status?", Vec::new()).unwrap();
        send(
            &mut fixture,
            &parent,
            Update::TurnEnded(StopReason::EndTurn),
        );

        assert_eq!(
            prompts(&fixture.backend, &parent),
            [
                "Status?".to_owned(),
                result_text(
                    &child,
                    TurnOrigin::Orchestrator,
                    &ChildOutcome::Ended(StopReason::Cancelled),
                    "half"
                )
            ]
        );
    }

    #[test]
    fn a_turn_that_ends_in_a_closed_project_sends_nothing_deferred() {
        let Family {
            mut fixture,
            project,
            parent,
            child,
            ..
        } = family();
        fixture.app.select_session(parent.clone()).unwrap();
        fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
        finish(&mut fixture, &child, "api done");
        fixture.app.close_project(project).unwrap();

        send(
            &mut fixture,
            &parent,
            Update::TurnEnded(StopReason::EndTurn),
        );

        assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
        assert_eq!(deferred(&fixture.app, &parent), 1);
        assert_eq!(status(&fixture.app, &parent), Status::Done);
    }

    #[test]
    fn closing_a_project_with_a_waiting_child_prompt_starts_no_turn() {
        let Family {
            mut fixture,
            project,
            parent,
            ..
        } = family();
        let (child, _) = unattached_child(&mut fixture, &project, &parent);
        orchestrate(&mut fixture, &child);

        fixture.app.close_project(project).unwrap();

        assert!(prompts(&fixture.backend, &parent).is_empty());
        assert_eq!(deferred(&fixture.app, &parent), 1);
        assert_eq!(status(&fixture.app, &child), Status::Idle);
    }

    #[test]
    fn a_cancelled_turn_reports_but_does_not_drain() {
        let Family {
            mut fixture,
            project,
            parent,
            child,
            ..
        } = family();
        let web = attached_child(&mut fixture, &project, &parent, "web", "Web page");
        fixture.app.select_session(parent.clone()).unwrap();
        fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
        finish(&mut fixture, &child, "api done");
        finish(&mut fixture, &web, "web done");

        send(
            &mut fixture,
            &parent,
            Update::TurnEnded(StopReason::Cancelled),
        );

        assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
        assert_eq!(deferred(&fixture.app, &parent), 2);

        fixture.app.submit_prompt("Go on", Vec::new()).unwrap();
        send(
            &mut fixture,
            &parent,
            Update::TurnEnded(StopReason::EndTurn),
        );

        assert_eq!(
            last_prompt(&fixture.backend, &parent),
            result_text(
                &child,
                TurnOrigin::Orchestrator,
                &ChildOutcome::Ended(StopReason::EndTurn),
                "api done"
            )
        );
        assert_eq!(deferred(&fixture.app, &parent), 1);
    }

    #[test]
    fn a_child_whose_connection_closes_mid_turn_reports_the_failure() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        orchestrate(&mut fixture, &child);
        send(&mut fixture, &child, Update::AgentText("half".into()));

        fixture
            .app
            .apply_event(BackendEvent::SessionClosed {
                session: child.clone(),
                error: "agent crashed".into(),
            })
            .unwrap();

        assert_eq!(
            last_prompt(&fixture.backend, &parent),
            result_text(
                &child,
                TurnOrigin::Orchestrator,
                &ChildOutcome::Failed("connection to the agent closed: agent crashed".into()),
                "half"
            )
        );
        assert_eq!(status(&fixture.app, &child), Status::Failed);
    }

    #[test]
    fn a_child_whose_create_fails_reports_the_failure() {
        let Family {
            mut fixture,
            project,
            parent,
            ..
        } = family();
        let (child, request) = unattached_child(&mut fixture, &project, &parent);
        orchestrate(&mut fixture, &child);

        fixture
            .app
            .apply_event(BackendEvent::RequestFailed {
                request,
                session: child.clone(),
                error: "agent not found".into(),
            })
            .unwrap();

        assert_eq!(
            last_prompt(&fixture.backend, &parent),
            result_text(
                &child,
                TurnOrigin::Orchestrator,
                &ChildOutcome::Failed("agent not found".into()),
                "(no final message)"
            )
        );
        assert_eq!(status(&fixture.app, &child), Status::Failed);
    }

    #[test]
    fn a_failed_create_outside_a_turn_reports_nothing() {
        let Family {
            mut fixture,
            project,
            parent,
            ..
        } = family();
        let (child, request) = unattached_child(&mut fixture, &project, &parent);

        fixture
            .app
            .apply_event(BackendEvent::RequestFailed {
                request,
                session: child.clone(),
                error: "agent not found".into(),
            })
            .unwrap();

        assert!(prompts(&fixture.backend, &parent).is_empty());
        assert_eq!(status(&fixture.app, &child), Status::Failed);
    }

    #[test]
    fn a_stopped_queued_child_prompt_reports_a_cancellation() {
        let Family {
            mut fixture,
            project,
            parent,
            ..
        } = family();
        let (child, _) = unattached_child(&mut fixture, &project, &parent);
        orchestrate(&mut fixture, &child);
        fixture.app.select_session(child.clone()).unwrap();

        fixture.app.cancel().unwrap();

        assert_eq!(
            last_prompt(&fixture.backend, &parent),
            result_text(
                &child,
                TurnOrigin::Orchestrator,
                &ChildOutcome::Ended(StopReason::Cancelled),
                "(no final message)"
            )
        );
        assert_eq!(status(&fixture.app, &child), Status::Idle);
    }

    #[test]
    fn a_parent_whose_connection_closed_drains_through_a_reattach() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        fixture.app.select_session(parent.clone()).unwrap();
        fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
        finish(&mut fixture, &child, "api done");
        assert_eq!(deferred(&fixture.app, &parent), 1);

        fixture
            .app
            .apply_event(BackendEvent::SessionClosed {
                session: parent.clone(),
                error: "agent crashed".into(),
            })
            .unwrap();

        let Some(Call::LoadSession {
            request,
            session,
            orchestrator: true,
            ..
        }) = fixture.backend.calls().last().cloned()
        else {
            panic!("the parent was not loaded again");
        };
        assert_eq!(session, parent);
        assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);

        fixture
            .app
            .apply_event(BackendEvent::SessionLoaded {
                request,
                session: parent.clone(),
                config: Vec::new(),
            })
            .unwrap();

        assert_eq!(
            last_prompt(&fixture.backend, &parent),
            result_text(
                &child,
                TurnOrigin::Orchestrator,
                &ChildOutcome::Ended(StopReason::EndTurn),
                "api done"
            )
        );
        assert_eq!(deferred(&fixture.app, &parent), 0);
        assert_eq!(status(&fixture.app, &parent), Status::Running);
    }

    #[test]
    fn a_generated_prompt_never_jumps_deferred_ones() {
        let Family {
            mut fixture,
            project,
            parent,
            child,
            ..
        } = family();
        let web = attached_child(&mut fixture, &project, &parent, "web", "Web page");
        fixture.app.select_session(parent.clone()).unwrap();
        fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
        finish(&mut fixture, &child, "api done");
        finish(&mut fixture, &web, "web done");
        send(
            &mut fixture,
            &parent,
            Update::TurnEnded(StopReason::Cancelled),
        );
        assert_eq!(status(&fixture.app, &parent), Status::Done);

        finish(&mut fixture, &child, "api done again");

        assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
        assert_eq!(deferred(&fixture.app, &parent), 3);

        fixture.app.submit_prompt("Go on", Vec::new()).unwrap();
        send(
            &mut fixture,
            &parent,
            Update::TurnEnded(StopReason::EndTurn),
        );

        assert_eq!(
            last_prompt(&fixture.backend, &parent),
            result_text(
                &child,
                TurnOrigin::Orchestrator,
                &ChildOutcome::Ended(StopReason::EndTurn),
                "api done"
            )
        );
        assert_eq!(deferred(&fixture.app, &parent), 2);
    }

    #[test]
    fn a_proposal_is_recorded_and_answered_at_once() {
        let Family {
            mut fixture,
            parent,
            ..
        } = family();
        fixture.app.select_session(parent.clone()).unwrap();
        fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
        let children = vec![
            proposed("api", "Add the endpoint", "API endpoint"),
            proposed("web", "Add the page", "Web page"),
        ];

        let answer = call_propose(&mut fixture, &parent, children.clone());

        assert_eq!(answer, Ok(PROPOSAL_RECORDED.to_owned()));
        assert_eq!(
            answer,
            Ok("proposal recorded, awaiting approval".to_owned())
        );
        let id = proposal_id(&fixture.app, &parent);
        assert_eq!(
            fixture.app.transcripts[&parent].entries().last(),
            Some(&Entry::Proposal {
                id: id.clone(),
                children: children.clone(),
                decision: None,
            })
        );
        assert_eq!(
            fixture.store.load_updates(parent.clone()).unwrap().last(),
            Some(&Update::ChildrenProposed {
                id: id.clone(),
                children: children.clone(),
            })
        );
        assert_eq!(
            fixture.app.proposals.get(&(parent.clone(), id)),
            Some(&children)
        );
        assert_eq!(status(&fixture.app, &parent), Status::Running);
    }

    #[test]
    fn a_proposal_for_an_unknown_repository_is_refused_and_names_the_repositories() {
        let Family {
            dir,
            mut fixture,
            parent,
            ..
        } = family();

        let answer = call_propose(
            &mut fixture,
            &parent,
            vec![
                proposed("api", "Add the endpoint", "API endpoint"),
                proposed("apu", "Add the page", "Web page"),
            ],
        );

        assert_eq!(
            answer,
            Err("unknown repository \"apu\"; the repositories are: api, web".to_owned())
        );
        assert!(fixture.app.transcripts[&parent].entries().is_empty());
        assert!(fixture.app.proposals.is_empty());

        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fixture.app.open_project(dir.path()).unwrap();
        let root = fixture.app.projects[0].repositories[0].clone();
        assert!(root.is_root);

        let answer = call_propose(
            &mut fixture,
            &parent,
            vec![proposed(&root.name, "Fix it", "Root")],
        );

        assert_eq!(
            answer,
            Err(format!(
                "unknown repository \"{}\"; the repositories are: ",
                root.name
            ))
        );
        assert!(fixture.app.transcripts[&parent].entries().is_empty());
    }

    #[test]
    fn an_empty_proposal_and_a_blank_prompt_are_refused() {
        let Family {
            mut fixture,
            parent,
            ..
        } = family();

        let empty = call_propose(&mut fixture, &parent, Vec::new());
        let blank = call_propose(
            &mut fixture,
            &parent,
            vec![
                proposed("api", "Add the endpoint", "API endpoint"),
                proposed("web", " \n ", "Web page"),
            ],
        );

        assert_eq!(
            empty,
            Err("propose_children needs at least one child".to_owned())
        );
        assert_eq!(blank, Err("child 2 has an empty prompt".to_owned()));
        assert!(fixture.app.transcripts[&parent].entries().is_empty());
        assert!(fixture.app.proposals.is_empty());
    }

    #[test]
    fn a_blank_title_is_derived_from_the_prompt() {
        let Family {
            mut fixture,
            parent,
            ..
        } = family();

        let answer = call_propose(
            &mut fixture,
            &parent,
            vec![proposed("api", "\nAdd the endpoint\nwith tests", "  ")],
        );

        assert_eq!(answer, Ok(PROPOSAL_RECORDED.to_owned()));
        assert_eq!(
            fixture.app.transcripts[&parent].entries(),
            [Entry::Proposal {
                id: proposal_id(&fixture.app, &parent),
                children: vec![proposed(
                    "api",
                    "\nAdd the endpoint\nwith tests",
                    "Add the endpoint"
                )],
                decision: None,
            }]
        );
    }

    #[test]
    fn tool_calls_from_non_orchestrators_and_unknown_sessions_are_refused() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        let children = vec![proposed("api", "Add the endpoint", "API endpoint")];

        let repository_session = call_propose(&mut fixture, &child, children.clone());
        let unknown = call_propose(&mut fixture, &SessionId::new(), children.clone());
        fixture
            .app
            .apply_event(BackendEvent::SessionClosed {
                session: parent.clone(),
                error: "agent crashed".into(),
            })
            .unwrap();
        let detached = call_send_to_child(&mut fixture, &parent, &child, "Also add tests");

        assert_eq!(
            repository_session,
            Err("this session is not an orchestrator".to_owned())
        );
        assert_eq!(unknown, Err("unknown session".to_owned()));
        assert_eq!(detached, Err("unknown session".to_owned()));
        assert!(fixture.app.transcripts[&child].entries().is_empty());
        assert!(fixture.app.proposals.is_empty());
        assert!(prompts(&fixture.backend, &child).is_empty());
    }

    #[test]
    fn an_orchestrator_with_an_undecided_proposal_waits_for_you_when_its_turn_ends() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        fixture.app.select_session(parent.clone()).unwrap();
        fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
        let children = vec![proposed("web", "Add the page", "Web page")];
        call_propose(&mut fixture, &parent, children).unwrap();
        finish(&mut fixture, &child, "api done");

        send(
            &mut fixture,
            &parent,
            Update::TurnEnded(StopReason::EndTurn),
        );

        assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);
        assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
        assert_eq!(deferred(&fixture.app, &parent), 1);
        assert!(matches!(
            fixture.app.submit_prompt("Go on", Vec::new()),
            Err(AppError::TurnInProgress)
        ));
        assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
    }

    #[test]
    fn send_to_child_prompts_an_idle_child_now() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();

        let answer = call_send_to_child(&mut fixture, &parent, &child, "Also add tests");

        assert_eq!(answer, Ok(format!("prompt sent to child {child}")));
        assert_eq!(prompts(&fixture.backend, &child), ["Also add tests"]);
        assert_eq!(
            fixture.app.transcripts[&child].entries().last(),
            Some(&Entry::OrchestratorPrompt("Also add tests".into()))
        );
        assert_eq!(status(&fixture.app, &child), Status::Running);
    }

    #[test]
    fn send_to_child_queues_for_a_busy_child_until_its_turn_ends() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        orchestrate(&mut fixture, &child);
        send(&mut fixture, &child, Update::AgentText("added".into()));

        let answer = call_send_to_child(&mut fixture, &parent, &child, "Also add tests");

        assert_eq!(
            answer,
            Ok(format!(
                "child {child} is mid-turn; the prompt goes out when its turn ends"
            ))
        );
        assert_eq!(prompts(&fixture.backend, &child), ["Add the endpoint"]);

        send(&mut fixture, &child, Update::TurnEnded(StopReason::EndTurn));

        assert_eq!(
            prompts(&fixture.backend, &parent),
            [result_text(
                &child,
                TurnOrigin::Orchestrator,
                &ChildOutcome::Ended(StopReason::EndTurn),
                "added"
            )]
        );
        assert_eq!(
            prompts(&fixture.backend, &child),
            ["Add the endpoint", "Also add tests"]
        );
        assert_eq!(
            fixture.app.transcripts[&child].entries().last(),
            Some(&Entry::OrchestratorPrompt("Also add tests".into()))
        );
        assert_eq!(status(&fixture.app, &child), Status::Running);
    }

    #[test]
    fn send_to_child_refuses_strangers() {
        let Family {
            mut fixture,
            project,
            parent,
            ..
        } = family();
        let stranger =
            attached_session(&mut fixture, &project, Scope::Repository(repository("web")));
        let unknown = SessionId::new();

        let to_unknown = call_send_to_child(&mut fixture, &parent, &unknown, "Also add tests");
        let to_stranger = call_send_to_child(&mut fixture, &parent, &stranger, "Also add tests");

        assert_eq!(to_unknown, Err(format!("unknown child {unknown}")));
        assert_eq!(
            to_stranger,
            Err(format!("{stranger} is not a child of this session"))
        );
        assert!(prompts(&fixture.backend, &stranger).is_empty());
        assert_eq!(status(&fixture.app, &stranger), Status::Idle);
    }

    fn choice(name: &str, prompt: &str, title: &str, auto_allow: bool) -> ChildChoice {
        ChildChoice {
            repository: repository(name),
            prompt: prompt.into(),
            title: title.into(),
            auto_allow,
        }
    }

    /// Every create request the backend received: its id, session, working directory and
    /// whether the session is an orchestrator.
    fn creates(backend: &RecordingBackend) -> Vec<(RequestId, SessionId, PathBuf, bool)> {
        backend
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::NewSession {
                    request,
                    session,
                    cwd,
                    orchestrator,
                    ..
                } => Some((request, session, cwd, orchestrator)),
                _ => None,
            })
            .collect()
    }

    /// Lets the orchestrator `parent` propose `children` and returns the proposal's id.
    fn propose_now(
        fixture: &mut Fixture,
        parent: &SessionId,
        children: Vec<ProposedChild>,
    ) -> ProposalId {
        call_propose(fixture, parent, children).unwrap();
        proposal_id(&fixture.app, parent)
    }

    /// The two children the tests propose by default.
    fn two_children() -> Vec<ProposedChild> {
        vec![
            proposed("api", "Add the endpoint", "API endpoint"),
            proposed("web", "Add the page", "Web page"),
        ]
    }

    /// The decision recorded on the proposal `id` in the session's transcript.
    fn decision(app: &App, session: &SessionId, id: &ProposalId) -> Option<Decision> {
        app.transcripts[session]
            .entries()
            .iter()
            .find_map(|entry| match entry {
                Entry::Proposal {
                    id: existing,
                    decision,
                    ..
                } if existing == id => Some(decision.clone()),
                _ => None,
            })
            .flatten()
    }

    /// The children recorded as approved on the proposal `id`; none when it is not approved.
    fn approved(app: &App, session: &SessionId, id: &ProposalId) -> Vec<ApprovedChild> {
        app.transcripts[session]
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                Entry::Proposal {
                    id: existing,
                    decision: Some(Decision::Approved(children)),
                    ..
                } if existing == id => Some(children.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// The message of an [`AppError::InvalidChildren`], `None` for any other result.
    fn invalid(result: Result<Vec<SessionId>, AppError>) -> Option<String> {
        result
            .err()
            .filter(|error| matches!(error, AppError::InvalidChildren(_)))
            .map(|error| error.to_string())
    }

    /// Lets the orchestrator `parent` start a turn, propose `children` and end the turn, so it
    /// waits for the decision; returns the proposal's id.
    fn waiting_on_the_card(
        fixture: &mut Fixture,
        parent: &SessionId,
        children: Vec<ProposedChild>,
    ) -> ProposalId {
        fixture.app.select_session(parent.clone()).unwrap();
        fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
        let id = propose_now(fixture, parent, children);
        send(fixture, parent, Update::TurnEnded(StopReason::EndTurn));
        assert_eq!(status(&fixture.app, parent), Status::WaitingForYou);
        id
    }

    /// Answers the create request `request` for `session`.
    fn create(fixture: &mut Fixture, request: RequestId, session: &SessionId) {
        fixture
            .app
            .apply_event(BackendEvent::SessionCreated {
                request,
                session: session.clone(),
                backend_session: BackendSessionId::new(format!("b-{session}")),
                config: Vec::new(),
            })
            .unwrap();
    }

    /// Asserts that `child` was started as approved, as a child of `parent`, and that its
    /// prompt waits for its backend session.
    fn assert_started(fixture: &Fixture, parent: &SessionId, child: &ApprovedChild) {
        let session = fixture.app.session(&child.session).unwrap();
        assert_eq!(session.parent.as_ref(), Some(parent));
        assert_eq!(session.scope, Scope::Repository(child.repository.clone()));
        assert_eq!(session.title, child.title);
        assert_eq!(session.auto_allow, child.auto_allow);
        assert_eq!(session.status, Status::Running);
        assert_eq!(
            fixture.app.transcripts[&child.session].entries(),
            [Entry::OrchestratorPrompt(child.prompt.clone())]
        );
        assert!(prompts(&fixture.backend, &child.session).is_empty());
    }

    /// Asserts that `child` was sent its prompt after its repository's instruction chain.
    fn assert_prompted_with_instructions(
        fixture: &Fixture,
        project: &ProjectId,
        child: &ApprovedChild,
    ) {
        let project = fixture.app.project(project).unwrap();
        let repository = project
            .repositories
            .iter()
            .find(|repository| repository.id == child.repository)
            .unwrap();
        assert_eq!(
            prompts(&fixture.backend, &child.session),
            [inject(
                &instruction_chain(project, Some(repository)),
                &child.prompt
            )]
        );
    }

    #[test]
    fn approving_starts_one_child_per_entry_and_wakes_the_orchestrator() {
        let Family {
            dir,
            mut fixture,
            project,
            parent,
            ..
        } = family();
        fs::write(
            dir.path().join("api/AGENTS.md"),
            "Keep the API compatible.\n",
        )
        .unwrap();
        let id = propose_now(&mut fixture, &parent, two_children());
        let before = creates(&fixture.backend).len();
        let current = fixture.app.current_session();

        let children = fixture
            .app
            .approve_proposal(
                parent.clone(),
                id.clone(),
                vec![
                    choice("api", "Add the endpoint with tests", "API endpoint", false),
                    choice("web", "Add the page", "Web page", true),
                ],
            )
            .unwrap();

        let created = creates(&fixture.backend).split_off(before);
        assert_eq!(
            created
                .iter()
                .map(|(_, session, _, _)| session.clone())
                .collect::<Vec<_>>(),
            children
        );
        for ((_, _, cwd, orchestrator), name) in created.iter().zip(["api", "web"]) {
            let scope = Scope::Repository(repository(name));
            assert_eq!(
                cwd,
                &fixture.app.working_directory(&project, &scope).unwrap()
            );
            assert!(!orchestrator);
        }
        let expected = vec![
            ApprovedChild {
                session: children[0].clone(),
                repository: repository("api"),
                prompt: "Add the endpoint with tests".into(),
                title: "API endpoint".into(),
                auto_allow: false,
            },
            ApprovedChild {
                session: children[1].clone(),
                repository: repository("web"),
                prompt: "Add the page".into(),
                title: "Web page".into(),
                auto_allow: true,
            },
        ];
        for child in &expected {
            assert_started(&fixture, &parent, child);
        }
        let wake = children_started_prompt(&expected);
        assert_eq!(
            fixture.app.transcripts[&parent].entries(),
            [
                Entry::Proposal {
                    id: id.clone(),
                    children: two_children(),
                    decision: Some(Decision::Approved(expected.clone())),
                },
                Entry::PartiturPrompt(wake.clone()),
            ]
        );
        assert_eq!(
            fixture.backend.calls().last(),
            Some(&Call::Prompt {
                session: parent.clone(),
                content: vec![PromptContent::Text(wake)],
            })
        );
        assert_eq!(status(&fixture.app, &parent), Status::Running);
        assert!(fixture.app.proposals.is_empty());
        assert_eq!(fixture.app.current_session(), current);

        for (request, child, _, _) in &created {
            create(&mut fixture, *request, child);
        }

        for child in &expected {
            assert_prompted_with_instructions(&fixture, &project, child);
        }
        assert!(
            prompts(&fixture.backend, &children[0])[0].contains("Keep the API compatible."),
            "the api child's prompt lacks its instructions"
        );
    }

    #[test]
    fn the_children_take_the_card_values_not_the_proposal() {
        let Family {
            mut fixture,
            parent,
            ..
        } = family();
        let id = propose_now(&mut fixture, &parent, two_children());

        let children = fixture
            .app
            .approve_proposal(
                parent.clone(),
                id.clone(),
                vec![
                    choice("web", "Add the page", "Web page", false),
                    choice("web", "\nStyle the page\nwith care", "  ", false),
                ],
            )
            .unwrap();

        let approved = approved(&fixture.app, &parent, &id);
        assert_eq!(
            approved
                .iter()
                .map(|child| (child.repository.as_str(), child.title.as_str()))
                .collect::<Vec<_>>(),
            [("web", "Web page"), ("web", "Style the page")]
        );
        assert_eq!(
            approved
                .iter()
                .map(|child| child.session.clone())
                .collect::<Vec<_>>(),
            children
        );
        let child = fixture.app.session(&children[1]).unwrap();
        assert_eq!(child.title, "Style the page");
        assert_eq!(child.scope, Scope::Repository(repository("web")));
        assert_eq!(
            fixture.app.transcripts[&children[1]].entries(),
            [Entry::OrchestratorPrompt(
                "\nStyle the page\nwith care".into()
            )]
        );
    }

    #[test]
    fn an_orchestrator_waiting_on_the_card_runs_again_after_the_decision() {
        let Family {
            mut fixture,
            parent,
            ..
        } = family();
        let id = waiting_on_the_card(&mut fixture, &parent, two_children());

        fixture
            .app
            .approve_proposal(
                parent.clone(),
                id.clone(),
                vec![choice("api", "Add the endpoint", "API endpoint", false)],
            )
            .unwrap();

        assert_eq!(status(&fixture.app, &parent), Status::Running);
        assert_eq!(
            prompts(&fixture.backend, &parent),
            [
                "Plan it".to_owned(),
                children_started_prompt(&approved(&fixture.app, &parent, &id))
            ]
        );
        assert_eq!(deferred(&fixture.app, &parent), 0);
    }

    #[test]
    fn an_orchestrator_mid_turn_gets_the_wake_after_its_turn() {
        let Family {
            mut fixture,
            parent,
            ..
        } = family();
        fixture.app.select_session(parent.clone()).unwrap();
        fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
        let id = propose_now(&mut fixture, &parent, two_children());

        fixture
            .app
            .approve_proposal(
                parent.clone(),
                id.clone(),
                vec![choice("api", "Add the endpoint", "API endpoint", false)],
            )
            .unwrap();

        assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
        assert_eq!(deferred(&fixture.app, &parent), 1);
        assert_eq!(status(&fixture.app, &parent), Status::Running);

        send(
            &mut fixture,
            &parent,
            Update::TurnEnded(StopReason::EndTurn),
        );

        let wake = children_started_prompt(&approved(&fixture.app, &parent, &id));
        assert_eq!(last_prompt(&fixture.backend, &parent), wake);
        assert_eq!(
            fixture.app.transcripts[&parent].entries().last(),
            Some(&Entry::PartiturPrompt(wake))
        );
        assert_eq!(status(&fixture.app, &parent), Status::Running);
    }

    #[test]
    fn invalid_children_change_nothing() {
        let Family {
            mut fixture,
            parent,
            ..
        } = family();
        let id = propose_now(&mut fixture, &parent, two_children());
        let calls = fixture.backend.calls();
        let sessions = fixture.app.sessions.len();

        let empty = invalid(
            fixture
                .app
                .approve_proposal(parent.clone(), id.clone(), Vec::new()),
        );
        let unknown = invalid(fixture.app.approve_proposal(
            parent.clone(),
            id.clone(),
            vec![choice("apu", "Add the endpoint", "API endpoint", false)],
        ));
        let blank = invalid(fixture.app.approve_proposal(
            parent.clone(),
            id.clone(),
            vec![
                choice("api", "Add the endpoint", "API endpoint", false),
                choice("web", " \n ", "Web page", true),
            ],
        ));

        assert_eq!(
            empty.as_deref(),
            Some("cannot start these children: propose_children needs at least one child")
        );
        assert_eq!(
            unknown.as_deref(),
            Some(
                "cannot start these children: unknown repository \"apu\"; the repositories are: api, web"
            )
        );
        assert_eq!(
            blank.as_deref(),
            Some("cannot start these children: child 2 has an empty prompt")
        );
        assert_eq!(fixture.backend.calls(), calls);
        assert_eq!(fixture.app.sessions.len(), sessions);
        assert_eq!(
            fixture.app.transcripts[&parent].pending_proposal(&id),
            Some(two_children().as_slice())
        );
        assert!(fixture.app.proposals.contains_key(&(parent.clone(), id)));
    }

    #[test]
    fn a_backend_failure_while_approving_leaves_the_proposal_undecided() {
        let Family {
            mut fixture,
            parent,
            ..
        } = family();
        let id = propose_now(&mut fixture, &parent, two_children());
        let sessions = fixture.app.sessions.len();
        fixture.backend.fail_next_call();

        let result = fixture.app.approve_proposal(
            parent.clone(),
            id.clone(),
            vec![choice("api", "Add the endpoint", "API endpoint", false)],
        );

        assert!(matches!(result, Err(AppError::Backend(_))), "{result:?}");
        assert_eq!(fixture.app.sessions.len(), sessions);
        assert_eq!(
            fixture.app.transcripts[&parent].pending_proposal(&id),
            Some(two_children().as_slice())
        );
        assert!(fixture.app.proposals.contains_key(&(parent.clone(), id)));
        assert!(prompts(&fixture.backend, &parent).is_empty());
    }

    #[test]
    fn rejecting_wakes_the_orchestrator_with_the_note() {
        let Family {
            mut fixture,
            parent,
            ..
        } = family();
        let id = propose_now(&mut fixture, &parent, two_children());
        let created = creates(&fixture.backend).len();

        fixture
            .app
            .reject_proposal(parent.clone(), id.clone(), Some("not now".into()))
            .unwrap();

        assert_eq!(
            decision(&fixture.app, &parent, &id),
            Some(Decision::Rejected {
                note: Some("not now".into())
            })
        );
        assert_eq!(
            last_prompt(&fixture.backend, &parent),
            "proposal rejected by the user\nnote: not now"
        );
        assert_eq!(
            fixture.app.transcripts[&parent].entries().last(),
            Some(&Entry::PartiturPrompt(
                "proposal rejected by the user\nnote: not now".into()
            ))
        );
        assert!(fixture.app.proposals.is_empty());
        assert_eq!(creates(&fixture.backend).len(), created);

        send(
            &mut fixture,
            &parent,
            Update::TurnEnded(StopReason::EndTurn),
        );
        let blank = propose_now(&mut fixture, &parent, two_children());

        fixture
            .app
            .reject_proposal(parent.clone(), blank.clone(), Some(" \n".into()))
            .unwrap();

        assert_eq!(
            decision(&fixture.app, &parent, &blank),
            Some(Decision::Rejected { note: None })
        );
        assert_eq!(
            last_prompt(&fixture.backend, &parent),
            "proposal rejected by the user"
        );
    }

    #[test]
    fn a_decided_proposal_cannot_be_approved_again() {
        let Family {
            dir: _dir,
            mut fixture,
            parent,
            ..
        } = family();
        let id = propose_now(&mut fixture, &parent, two_children());
        let card = vec![choice("api", "Add the endpoint", "API endpoint", false)];
        fixture
            .app
            .approve_proposal(parent.clone(), id.clone(), card.clone())
            .unwrap();
        let created = creates(&fixture.backend).len();

        let again = fixture
            .app
            .approve_proposal(parent.clone(), id.clone(), card.clone());
        let rejected = fixture
            .app
            .reject_proposal(parent.clone(), id.clone(), None);
        let stranger = fixture
            .app
            .approve_proposal(SessionId::new(), id.clone(), card.clone());

        assert!(matches!(&again, Err(AppError::UnknownProposal(refused)) if *refused == id));
        assert_eq!(
            again.unwrap_err().to_string(),
            format!("no undecided proposal {id}")
        );
        assert!(matches!(rejected, Err(AppError::UnknownProposal(refused)) if refused == id));
        assert!(matches!(stranger, Err(AppError::UnknownSession(_))));
        assert_eq!(creates(&fixture.backend).len(), created);

        let mut restarted = fixture_with(fixture.store.clone());
        drop(fixture);
        restarted.app.select_session(parent.clone()).unwrap();

        let after_restart = restarted
            .app
            .approve_proposal(parent.clone(), id.clone(), card);

        assert!(matches!(after_restart, Err(AppError::UnknownProposal(refused)) if refused == id));
        assert!(creates(&restarted.backend).is_empty());
    }

    #[test]
    fn a_pending_proposal_survives_a_restart() {
        let Family {
            dir: _dir,
            mut fixture,
            parent,
            ..
        } = family();
        let id = propose_now(&mut fixture, &parent, two_children());
        let mut restarted = fixture_with(fixture.store.clone());
        drop(fixture);
        assert!(restarted.app.proposals.is_empty());
        restarted.app.select_session(parent.clone()).unwrap();

        let children = restarted
            .app
            .approve_proposal(
                parent.clone(),
                id.clone(),
                vec![choice("web", "Add the page", "Web page", false)],
            )
            .unwrap();

        let created = creates(&restarted.backend);
        assert_eq!(created.len(), 1);
        assert_eq!(vec![created[0].1.clone()], children);
        assert_eq!(
            restarted.app.session(&children[0]).unwrap().parent,
            Some(parent.clone())
        );
        assert_eq!(approved(&restarted.app, &parent, &id).len(), 1);
        assert_eq!(
            restarted.app.transcripts[&parent].entries().last(),
            Some(&Entry::PartiturPrompt(children_started_prompt(&approved(
                &restarted.app,
                &parent,
                &id
            ))))
        );
        assert_eq!(status(&restarted.app, &parent), Status::Running);
    }

    #[test]
    fn new_session_still_selects_and_titles_as_before() {
        let dir = project_dir(&["api", "web"]);
        let mut fixture = fixture_with(MemoryStore::new());
        let project = fixture.app.open_project(dir.path()).unwrap();

        let session = fixture
            .app
            .new_session(project.clone(), Scope::Repository(repository("api")))
            .unwrap();

        assert_eq!(fixture.app.current_session(), Some(session.clone()));
        let created = fixture.app.session(&session).unwrap();
        assert_eq!(created.title, "New session");
        assert_eq!(created.parent, None);
        assert!(!created.auto_allow);
        assert!(!created.orchestrator);
        assert_eq!(created.status, Status::Idle);
        assert_eq!(
            fixture.store.load_sessions(project).unwrap(),
            std::slice::from_ref(created)
        );
        assert_eq!(fixture.app.transcripts[&session].entries(), []);
    }

    #[test]
    fn a_decision_starts_the_deferred_results_before_the_wake() {
        let Family {
            mut fixture,
            project,
            parent,
            child,
            ..
        } = family();
        let web = attached_child(&mut fixture, &project, &parent, "web", "Web page");
        let id = waiting_on_the_card(&mut fixture, &parent, two_children());
        finish(&mut fixture, &child, "api done");
        finish(&mut fixture, &web, "web done");
        assert_eq!(deferred(&fixture.app, &parent), 2);

        fixture
            .app
            .approve_proposal(
                parent.clone(),
                id.clone(),
                vec![choice("api", "Add the endpoint", "API endpoint", false)],
            )
            .unwrap();

        assert_eq!(
            last_prompt(&fixture.backend, &parent),
            result_text(
                &child,
                TurnOrigin::Orchestrator,
                &ChildOutcome::Ended(StopReason::EndTurn),
                "api done"
            )
        );
        assert_eq!(deferred(&fixture.app, &parent), 2);
        assert_eq!(
            fixture.app.deferred[&parent].back(),
            Some(&Generated::Wake(children_started_prompt(&approved(
                &fixture.app,
                &parent,
                &id
            ))))
        );
        assert_eq!(status(&fixture.app, &parent), Status::Running);
    }

    #[test]
    fn another_undecided_proposal_keeps_the_orchestrator_waiting() {
        let Family {
            mut fixture,
            parent,
            ..
        } = family();
        fixture.app.select_session(parent.clone()).unwrap();
        fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
        let first = propose_now(&mut fixture, &parent, two_children());
        call_propose(
            &mut fixture,
            &parent,
            vec![proposed("web", "Add the page", "Web page")],
        )
        .unwrap();
        send(
            &mut fixture,
            &parent,
            Update::TurnEnded(StopReason::EndTurn),
        );

        fixture
            .app
            .reject_proposal(parent.clone(), first, None)
            .unwrap();

        assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);
        assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
        assert_eq!(deferred(&fixture.app, &parent), 1);

        let second = proposal_id(&fixture.app, &parent);
        fixture
            .app
            .reject_proposal(parent.clone(), second, Some("later".into()))
            .unwrap();

        assert_eq!(status(&fixture.app, &parent), Status::Running);
        assert_eq!(
            prompts(&fixture.backend, &parent),
            ["Plan it", "proposal rejected by the user"]
        );
        assert_eq!(
            fixture.app.deferred[&parent],
            [Generated::Wake(
                "proposal rejected by the user\nnote: later".into()
            )]
        );
    }

    #[test]
    fn a_waiting_permission_keeps_the_orchestrator_waiting() {
        let Family {
            mut fixture,
            parent,
            ..
        } = family();
        fixture.app.select_session(parent.clone()).unwrap();
        fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
        let id = propose_now(&mut fixture, &parent, two_children());
        send(
            &mut fixture,
            &parent,
            Update::PermissionRequested {
                id: PermissionId(1),
                tool_call_id: ToolCallId::new("t-1"),
                title: "Read main.rs".into(),
                kind: ToolKind::Read,
                options: vec![PermissionOption {
                    id: PermissionOptionId::new("allow"),
                    name: "Allow".into(),
                    kind: PermissionKind::AllowOnce,
                }],
                name: None,
            },
        );
        assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);

        fixture
            .app
            .reject_proposal(parent.clone(), id, None)
            .unwrap();

        assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);
        assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
        assert_eq!(deferred(&fixture.app, &parent), 1);
    }

    #[test]
    fn a_tool_call_that_fails_inside_is_answered_with_the_error() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        fixture.backend.fail_next_call();

        let result = fixture.app.apply_event(BackendEvent::ToolCalled {
            call: ToolRequestId(7),
            session: parent.clone(),
            tool: OrchestratorTool::SendToChild {
                child: child.clone(),
                prompt: "Also add tests".into(),
            },
        });

        assert!(matches!(result, Err(AppError::Backend(_))), "{result:?}");
        let error = result.unwrap_err();
        let calls = fixture.backend.calls();
        assert!(matches!(
            &calls[calls.len() - 2],
            Call::Prompt { session, .. } if *session == child
        ));
        assert_eq!(
            calls.last(),
            Some(&Call::AnswerTool {
                call: ToolRequestId(7),
                result: Err(error.to_string()),
            })
        );
    }
}
