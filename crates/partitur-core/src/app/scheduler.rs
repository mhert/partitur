//! The prompts Partitur writes to a session on its own: the wake of an orchestrator, a prompt
//! an orchestrator sends its child, a child's result and a child's title proposal. A child's
//! finished turn reaches its parent as a prompt. Generated prompts for a busy session wait for
//! its turn to end and then go out one per turn, in the order they came, but for one
//! exception: a child's newer title proposal takes the place of its older one still waiting,
//! and a title proposal that goes out from the queue carries the child's title as it is then.
//!
//! A session is parked, and its generated prompts wait, while its project is closed, while it
//! is archived, while it waits at a gate of its own and while it has a question open; they go
//! out once it is reopened or restored, or its gate was decided or its question answered or
//! dismissed.
//!
//! Every child result carries where the child stands in its workflow. A child whose turn ends
//! at a gate or with a question open does not wait for the user while it has a live
//! orchestrator: the orchestrator hears at once, with a `child waits` prompt.
//!
//! Such a child [waits for its orchestrator](App::waits_for_orchestrator) rather than for the
//! user, so the tree and `G` do not count it as asking the user, unless a permission request
//! of it waits: that stays the user's to answer.
//!
//! The user can still decide in the child: that drops the child's `child waits` still queued.
//! An orchestrator with a question to the user open is parked so until the question is
//! answered or dismissed. A child that waits while its orchestrator is archived
//! keeps its wait for the user until the orchestrator is restored, which then hears of it.
//! Which wait went out is stored with the child, so after a restart a wait still open that
//! never reached the orchestrator is reported, and one that did is not reported twice.

use std::collections::HashSet;

use super::{App, AppError};
use crate::backend::{
    ChildOutcome, ChildState, ChildWait, StopReason, TitleProposal, TurnOrigin, Update,
};
use crate::ids::{RepositoryId, SessionId};
use crate::orchestrator::{child_result_prompt, title_proposal_prompt};
use crate::session::{Scope, Status, TitleSource, WaitMark};
use crate::transcript::{Entry, Transcript};
use crate::workflow;

/// What a child reports when its turn ended without an answer from the agent.
pub(super) const NO_FINAL_MESSAGE: &str = "(no final message)";

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

    /// Whether `session` waits for its orchestrator rather than for the user: it waits for
    /// you at a gate or with a question open, its orchestrator
    /// [decides for it](Self::delegates) and [hears of the wait](Self::wait_heard), and no
    /// permission request of it waits, which stays the user's to answer.
    pub fn waits_for_orchestrator(&self, session: &SessionId) -> bool {
        self.session(session)
            .is_ok_and(|waiting| waiting.status == Status::WaitingForYou)
            && (self.gate_open(session).is_some() || self.question_open(session))
            && self.delegates(session)
            && self.wait_heard(session)
            && !self.has_waiting_permission(session)
    }

    /// Whether the orchestrator of `child` heard, or will hear, of what the child waits for
    /// now: its last `child waits` told of this very wait, or one is queued for it. A child
    /// whose report failed, at start for instance, keeps its wait for the user instead.
    fn wait_heard(&self, child: &SessionId) -> bool {
        self.session(child).is_ok_and(|session| {
            self.wait_mark(child) == session.wait_reported
                || session
                    .parent
                    .as_ref()
                    .and_then(|parent| self.deferred_of(parent))
                    .is_some_and(|queue| queue.iter().any(|generated| is_wait_of(generated, child)))
        })
    }

    /// The sessions that [wait for their orchestrator](Self::waits_for_orchestrator).
    pub(super) fn delegated(&self) -> HashSet<SessionId> {
        self.sessions
            .iter()
            .map(|session| &session.id)
            .filter(|session| self.waits_for_orchestrator(session))
            .cloned()
            .collect()
    }

    /// Reports to every orchestrator the children of it that wait and were not reported yet,
    /// as [`report_waiting_children`](Self::report_waiting_children) does for one; meant to be
    /// called once, after [`load`](Self::load). It takes up a `child waits` that was queued
    /// when Partitur closed, and a wait an older version held for the user. A child whose
    /// report fails is skipped and keeps waiting for the user; the others are reported all
    /// the same.
    ///
    /// # Errors
    ///
    /// Returns the first error a child's report ran into, as
    /// [`report_to_parent`](Self::report_to_parent) describes them.
    pub fn report_waiting_at_start(&mut self) -> Result<(), AppError> {
        let orchestrators: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|session| session.orchestrator)
            .map(|session| session.id.clone())
            .collect();
        let mut first = None;
        for orchestrator in orchestrators {
            for child in self.unreported_waits(&orchestrator) {
                if let Err(error) = self.report_wait(&child) {
                    first.get_or_insert(error);
                }
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// Reports to `orchestrator` every child of it that waits and was not reported yet, as
    /// [`report_to_parent`](Self::report_to_parent) reports a turn that ended at a gate or
    /// with a question open. Such a child is not in a turn, waits for you with a gate or a
    /// question open, and its wait is neither the one whose `child waits` last started a turn
    /// of the orchestrator nor already queued for it. A child without an orchestrator that
    /// [decides for it](Self::delegates) still reports nothing.
    ///
    /// The origin and the stop reason of the child's last turn are not stored, so the report
    /// names the turn as started by the orchestrator and ended normally
    /// ([`TurnOrigin::Orchestrator`], [`StopReason::EndTurn`]).
    ///
    /// This takes up what a child held for the user while it had no live orchestrator.
    ///
    /// # Errors
    ///
    /// As [`report_to_parent`](Self::report_to_parent); the children after the one that
    /// failed are not reported then.
    pub(super) fn report_waiting_children(
        &mut self,
        orchestrator: &SessionId,
    ) -> Result<(), AppError> {
        for child in self.unreported_waits(orchestrator) {
            self.report_wait(&child)?;
        }
        Ok(())
    }

    /// The children of `orchestrator` that wait and were not reported yet, as
    /// [`report_waiting_children`](Self::report_waiting_children) describes them.
    fn unreported_waits(&self, orchestrator: &SessionId) -> Vec<SessionId> {
        let queue = self.deferred_of(orchestrator);
        self.sessions
            .iter()
            .filter(|child| {
                child.parent.as_ref() == Some(orchestrator) && child.status == Status::WaitingForYou
            })
            .filter(|child| self.in_turn(&child.id).is_ok_and(|busy| !busy))
            .filter(|child| {
                let mark = self.wait_mark(&child.id);
                mark.is_some() && mark != child.wait_reported
            })
            .filter(|child| {
                !queue.is_some_and(|queue| {
                    queue
                        .iter()
                        .any(|generated| is_wait_of(generated, &child.id))
                })
            })
            .map(|child| child.id.clone())
            .collect()
    }

    /// Reports the wait of `child` to its parent, as a turn the orchestrator started that
    /// ended normally.
    ///
    /// # Errors
    ///
    /// As [`report_to_parent`](Self::report_to_parent).
    fn report_wait(&mut self, child: &SessionId) -> Result<(), AppError> {
        self.report_to_parent(
            child,
            ChildOutcome::Ended(StopReason::EndTurn),
            TurnOrigin::Orchestrator,
        )
    }

    /// Drops the `child waits` prompts of `child` still queued for its parent: the wait they
    /// report was decided in the child.
    pub(super) fn drop_waits(&mut self, child: &SessionId) {
        let Some(parent) = self
            .session(child)
            .ok()
            .and_then(|session| session.parent.clone())
        else {
            return;
        };
        if let Some(live) = self.live_mut(&parent) {
            live.deferred
                .retain(|generated| !is_wait_of(generated, child));
        }
    }

    /// The mark of what `child` waits for now: its open question, or else its open gate;
    /// `None` when neither is open or the child does not exist.
    pub(super) fn wait_mark(&self, child: &SessionId) -> Option<WaitMark> {
        let session = self.session(child).ok()?;
        if let Some(question) = &session.question {
            return Some(WaitMark::Question(question.clone()));
        }
        let gate = session.workflow.as_ref()?.gate.as_ref()?;
        Some(WaitMark::Gate { since: gate.since })
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
            let queue = &mut self.live_entry(session).deferred;
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
        let Some(generated) = self
            .live_mut(session)
            .and_then(|live| live.deferred.pop_front())
        else {
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

    /// Settles the end of the session's turn, however it ended: git is read again for the
    /// Env tab when it shows the session, the session reports `outcome` to its parent with
    /// the turn's origin (the user's when none was recorded), then, when `drain` is set, its
    /// next deferred prompt goes out.
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
        self.recheck_env_git(session);
        let origin = self
            .live_mut(session)
            .and_then(|live| live.turn_origin.take())
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
    /// orchestrator decides the gate or answers the question. A `child waits` still queued
    /// when the user decides the gate or answers the question in the child is dropped. Without
    /// such a parent it reports nothing: the user decides the gate or answers the question,
    /// and the next turn that ends with neither open reports its own final message; once the
    /// orchestrator is restored, or on the next start, a wait still open reaches it after all
    /// ([`report_waiting_children`](Self::report_waiting_children),
    /// [`report_waiting_at_start`](Self::report_waiting_at_start)). A failed turn is reported
    /// as a child result whatever gate or question is open, with its step but no wait.
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
        self.transcript(id)
            .ok_or_else(|| AppError::UnknownSession(id.clone()))
    }

    /// Whether a generated prompt may start a turn of the session now: it is not in a turn,
    /// it is not [parked](Self::parked), and no older generated prompt waits for it.
    fn may_start_generated(&self, session: &SessionId) -> Result<bool, AppError> {
        let nothing_deferred = self.deferred_of(session).is_none();
        Ok(!self.in_turn(session)? && !self.parked(session)? && nothing_deferred)
    }

    /// Whether the session takes no generated prompt for now: its project is closed, the
    /// session is archived, it waits at a gate of its own, or it has a question open. Its
    /// generated prompts wait until it is reopened or restored, or its gate was decided or
    /// its question answered or dismissed: a child result would otherwise drive an
    /// orchestrator past its gate, a follow-up would drive a child past the gate or question
    /// its orchestrator is about to decide, and a turn either started could supersede the
    /// question and lose the pending answer.
    fn parked(&self, session: &SessionId) -> Result<bool, AppError> {
        let parked = self.session(session)?;
        Ok(self.project(&parked.project)?.closed
            || self.is_archived(session)?
            || self.gate_open(session).is_some()
            || parked.question.is_some())
    }

    /// Starts a turn of `session` with `generated`, as [`deliver`](Self::deliver) describes.
    /// A `child waits` prompt marks the child's wait as [reported](crate::Session::wait_reported)
    /// once it went out.
    pub(super) fn start_generated(
        &mut self,
        session: &SessionId,
        generated: &Generated,
    ) -> Result<(), AppError> {
        self.start_turn(
            session,
            &[generated.record()],
            generated.text(),
            Vec::new(),
            TurnOrigin::Orchestrator,
            None,
        )?;
        if let Generated::ChildResult { child, state, .. } = generated
            && state.waits.is_some()
        {
            let mark = self.wait_mark(child);
            self.change_session(child.clone(), |waiting| waiting.wait_reported = mark)?;
        }
        Ok(())
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

/// Whether `generated` is a `child waits` prompt of `child`.
fn is_wait_of(generated: &Generated, child: &SessionId) -> bool {
    matches!(generated, Generated::ChildResult { child: waiting, state, .. }
        if waiting == child && state.waits.is_some())
}

/// Whether `entry` is the prompt a turn starts with.
fn starts_turn(entry: &Entry) -> bool {
    matches!(
        entry,
        Entry::User { .. }
            | Entry::OrchestratorPrompt(_)
            | Entry::PartiturPrompt(_)
            | Entry::ChildResult { .. }
            | Entry::TitleProposal(_)
            | Entry::AgentTurn
    )
}

#[cfg(test)]
mod tests;
