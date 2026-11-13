//! The workflow reports of a session: an agent's reports are appended to the session's
//! history and folded into the view stored with the session, and that view is rebuilt from
//! the history whenever it has fallen behind.

use super::{App, AppError, unix_seconds};
use crate::backend::Update;
use crate::ids::SessionId;
use crate::tool_call::ToolAnswer;
use crate::transcript::Transcript;
use crate::workflow::{self, SuperpowersEvent, WorkflowEvent};

impl App {
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
