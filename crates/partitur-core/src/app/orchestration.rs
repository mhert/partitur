//! How Partitur talks to an orchestrator on its own: the orchestrator's tool calls are
//! answered, a child's finished turn reaches its parent as a prompt, and prompts Partitur
//! generates for a busy session wait for its turn to end.

use std::collections::VecDeque;

use super::{App, AppError};
use crate::backend::{
    ApprovedChild, ChildOutcome, Decision, OrchestratorTool, ProposedChild, TurnOrigin, Update,
};
use crate::ids::{BackendId, ProposalId, RepositoryId, SessionId, ToolRequestId};
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

/// The answer to a `send_to_child` call with a blank prompt.
const NO_PROMPT: &str = "send_to_child needs a prompt";

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
    /// The children are created and prompted one by one. Each is a new session in its
    /// repository, a child of `session` running on the orchestrator's backend (on the
    /// [default backend](Self::default_backend) when that is no longer configured), titled as
    /// chosen (a blank title is derived from the prompt) and granting permissions without
    /// asking when chosen; it is not selected. It is [`Running`](Status::Running) at once, and
    /// its prompt goes out, after its repository's instruction chain, as soon as its backend
    /// session is created. Once every child is started, the decision goes into the
    /// orchestrator's transcript, and the orchestrator is woken with the started children and
    /// their session ids: at once when it is idle or was waiting only for its proposals, else
    /// once its turn ends, and always after child results that arrived before the decision.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::UnknownProposal`] when the session's loaded transcript has no undecided
    /// proposal `proposal`, [`AppError::InvalidChildren`] when `children` is empty, names a
    /// repository that is not one of the project's or has a blank prompt (nothing changes in
    /// these cases), [`AppError::Backend`] when the backend cannot queue a command and
    /// [`AppError::Store`] when the change cannot be saved. A backend or store failure after
    /// the first child was started leaves the children started so far in place and the
    /// proposal undecided, so approving it again starts those children a second time.
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
        let backend = self.child_backend(&session)?;
        let mut approved = Vec::new();
        for (child, auto_allow) in checked.into_iter().zip(auto_allow) {
            let id = self.create_session(
                project.clone(),
                Scope::Repository(child.repository.clone()),
                Some(session.clone()),
                child.title.clone(),
                auto_allow,
                backend.clone(),
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
            | Entry::AgentTurn
    )
}

#[cfg(test)]
mod tests;
