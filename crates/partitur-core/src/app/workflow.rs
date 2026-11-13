//! The workflow reports of a session: an agent's reports are appended to the session's
//! history and folded into the view stored with the session, and that view is rebuilt from
//! the history whenever it has fallen behind. A gate the agent opens is closed only by the
//! user, by approving it or by prompting the session.

use super::{App, AppError, unix_seconds};
use crate::backend::{TurnOrigin, Update};
use crate::ids::SessionId;
use crate::tool_call::ToolAnswer;
use crate::transcript::Transcript;
use crate::workflow::{self, GateCloser, GateKind, SuperpowersEvent, WorkflowEvent};

impl App {
    /// Approves the design, plan or checkpoint the session waits at: the gate closes as
    /// approved by the user, and the session is prompted with the gate's fixed
    /// [approval text](GateKind::approval_text) as the user's message. Any session can be
    /// approved, selected or not; one whose backend session is not attached queues the prompt
    /// and is created or loaded, as [`submit_prompt`](Self::submit_prompt) describes. The
    /// gate closes only once the backend has taken the command.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::Archived`] when it is archived, [`AppError::UnknownBackend`] when its
    /// backend is no longer configured, [`AppError::TurnInProgress`] when it is in a turn,
    /// [`AppError::NoGate`] when no gate is open, [`AppError::Backend`] when the backend
    /// cannot queue the command (nothing changes in these cases, and the gate stays open) and
    /// [`AppError::Store`] when the change cannot be saved.
    pub fn approve_gate(&mut self, session: SessionId) -> Result<(), AppError> {
        self.check_promptable(&session)?;
        let Some(kind) = self.gate_open(&session) else {
            return Err(AppError::NoGate(session));
        };
        let text = kind.approval_text();
        self.start_turn(
            &session,
            &Update::UserText(text.to_owned()),
            text.to_owned(),
            Vec::new(),
            TurnOrigin::User,
            Some(GateCloser::Approve),
        )
    }

    /// The session that has waited longest at a gate: of the sessions in open projects that
    /// are not archived, the one whose gate opened first, the older session on a tie. `None`
    /// when no such session waits.
    pub fn next_gate(&self) -> Option<SessionId> {
        self.sessions
            .iter()
            .filter(|session| session.archived.is_none())
            .filter(|session| {
                self.project(&session.project)
                    .is_ok_and(|project| !project.closed)
            })
            .filter_map(|session| {
                let gate = session.workflow.as_ref()?.gate.as_ref()?;
                Some((gate.since, session.created_at, &session.id))
            })
            .min_by_key(|&(since, created_at, _)| (since, created_at))
            .map(|(_, _, id)| id.clone())
    }

    /// The kind of the gate the session waits at, `None` when no gate is open or the session
    /// does not exist.
    pub(super) fn gate_open(&self, session: &SessionId) -> Option<GateKind> {
        let gate = self
            .session(session)
            .ok()?
            .workflow
            .as_ref()?
            .gate
            .as_ref()?;
        Some(gate.kind)
    }

    /// Closes the session's open gate, closed `by` the user, in its history and its stored
    /// view; a session without an open gate is left alone.
    ///
    /// # Errors
    ///
    /// As [`record_workflow`](Self::record_workflow).
    pub(super) fn close_gate(
        &mut self,
        session: &SessionId,
        by: GateCloser,
    ) -> Result<(), AppError> {
        let Some(gate) = self.gate_open(session) else {
            return Ok(());
        };
        self.record_workflow(session, WorkflowEvent::GateClosed { gate, by })
    }

    /// Records an agent's workflow report and answers with the text that tells it whether to
    /// stop and wait.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when the session does not exist and
    /// [`AppError::Store`] when the report cannot be saved.
    pub(super) fn workflow_reported(
        &mut self,
        session: &SessionId,
        event: SuperpowersEvent,
    ) -> Result<ToolAnswer, AppError> {
        let answer = workflow::tool_answer(&event);
        self.record_workflow(session, WorkflowEvent::Superpowers(event))?;
        Ok(Ok(answer.into()))
    }

    /// Appends `event` to the session's history, stamped with the current time, then folds it
    /// into the view stored with the session. The history comes first: the view is only a
    /// cache of it and is rebuilt when a crash leaves it behind.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when the session does not exist and
    /// [`AppError::Store`] when the history or the session cannot be saved.
    pub(super) fn record_workflow(
        &mut self,
        session: &SessionId,
        event: WorkflowEvent,
    ) -> Result<(), AppError> {
        let at = unix_seconds();
        self.apply_update(
            session,
            &Update::Workflow {
                at,
                event: event.clone(),
            },
        )?;
        self.change_session(session.clone(), move |recorded| {
            recorded.workflow = Some(workflow::apply(recorded.workflow.take(), &event, at));
        })
    }

    /// Rebuilds the session's view from `updates` and saves it when it has applied a different
    /// number of workflow events than `transcript` holds.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when the session does not exist and
    /// [`AppError::Store`] when the session cannot be saved.
    pub(super) fn refresh_workflow(
        &mut self,
        session: &SessionId,
        transcript: &Transcript,
        updates: &[Update],
    ) -> Result<(), AppError> {
        let applied = self
            .session(session)?
            .workflow
            .as_ref()
            .map_or(0, |view| view.applied);
        if transcript.workflow_events() == applied {
            return Ok(());
        }
        let rebuilt = workflow::rebuild(updates.iter().filter_map(|update| match update {
            Update::Workflow { at, event } => Some((*at, event)),
            _ => None,
        }));
        self.change_session(session.clone(), |stale| stale.workflow = rebuilt)
    }
}

#[cfg(test)]
mod tests;
