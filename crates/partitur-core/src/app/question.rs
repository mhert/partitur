//! The questions a session asks the user: the `ask_user` tool records a question with its
//! options in the session's transcript and answers at once, so the agent can end its turn. The
//! user's answer is the session's next prompt, an option's label or text of their own, and
//! closes the question; so does any prompt the user types, and so does dismissing it. A
//! session with a question open waits for you once its turn ends, and so after a restart.
//! While a session's question is open it takes no generated prompt, neither an
//! orchestrator's child results nor a child's follow-ups from its orchestrator; answering it
//! lets the next one go out once the answer's turn ends, and dismissing it outside a turn
//! lets it go out at once. Closing a child's question drops the
//! `child waits` prompt still queued for its orchestrator.

use super::{App, AppError};
use crate::backend::TurnOrigin;
use crate::ids::{QuestionId, SessionId};
use crate::question::{QUESTION_RECORDED, Question, QuestionOutcome, check};
use crate::record::TranscriptRecord;
use crate::session::Status;
use crate::tool_call::ToolAnswer;
use crate::workflow::{Closing, Decider, GateCloser};

impl App {
    /// Answers the open question of `session` with `answer`: the answer is the user's next
    /// prompt, and the question closes as answered with it. A blank answer does nothing.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists, [`AppError::Archived`]
    /// when it is archived, [`AppError::UnknownBackend`] when its backend is no longer configured,
    /// [`AppError::TurnInProgress`] when it is in a turn, [`AppError::NoQuestion`] when no question
    /// is open and [`AppError::Backend`] when the backend cannot queue the prompt (nothing changes
    /// in these cases, and the question stays open).
    pub fn answer_question(&mut self, session: SessionId, answer: &str) -> Result<(), AppError> {
        self.check_promptable(&session)?;
        if !self.question_open(&session) {
            return Err(AppError::NoQuestion(session));
        }
        if answer.trim().is_empty() {
            return Ok(());
        }
        self.start_turn(
            &session,
            &[TranscriptRecord::UserText(answer.to_owned())],
            answer.to_owned(),
            Vec::new(),
            TurnOrigin::User,
            Some(Closing {
                by: GateCloser::Prompt,
                decider: Decider::User,
            }),
        )?;
        self.close_question(
            &session,
            QuestionOutcome::Answered(answer.to_owned()),
            Decider::User,
        )
    }

    /// Dismisses the open question of `session`: it closes as cancelled, and nothing is sent
    /// to the agent and nothing reported to a parent. A session that waited for you only for
    /// the question is [`Done`](Status::Done) afterwards. A session not in a turn then takes
    /// the oldest generated prompt its question held back, also after a stopped turn: the
    /// user acted on it.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists, [`AppError::NoQuestion`]
    /// when no question is open and [`AppError::Backend`] when the backend cannot queue the prompt
    /// that goes out.
    pub fn cancel_question(&mut self, session: SessionId) -> Result<(), AppError> {
        if !self.question_open(&session) {
            self.session(&session)?;
            return Err(AppError::NoQuestion(session));
        }
        self.close_question(&session, QuestionOutcome::Cancelled, Decider::User)?;
        let waited_only_for_the_question = self.session(&session)?.status == Status::WaitingForYou
            && self.gate_open(&session).is_none()
            && !self.awaits_decision(&session)
            && self.waiting_permissions(&session).is_empty();
        if waited_only_for_the_question {
            self.set_status(session.clone(), Status::Done)?;
        }
        if self.in_turn(&session)? {
            return Ok(());
        }
        self.deliver_next(&session)
    }

    /// The open question of the selected session, `None` when no session is selected or it
    /// asked none. Unlike [`session_view`](Self::session_view) it looks at no transcript, so
    /// it is cheap enough to ask on every frame, like
    /// [`current_workflow`](Self::current_workflow).
    pub fn current_question(&self) -> Option<&QuestionId> {
        self.session(self.current.as_ref()?).ok()?.question.as_ref()
    }

    /// Whether the session has a question the user has not closed yet.
    pub(super) fn question_open(&self, session: &SessionId) -> bool {
        self.session(session)
            .is_ok_and(|asked| asked.question.is_some())
    }

    /// `ask_user`: records `question` in the session's transcript and answers at once, without
    /// waiting for the user. An open question is superseded by it. A question that fails
    /// [`check`] is refused with the reason, and nothing is recorded.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when the session does not exist.
    pub(super) fn ask(
        &mut self,
        session: &SessionId,
        question: Question,
    ) -> Result<ToolAnswer, AppError> {
        if let Err(reason) = check(&question) {
            return Ok(Err(reason));
        }
        self.close_question(session, QuestionOutcome::Superseded, Decider::User)?;
        let id = QuestionId::new();
        self.apply_update(
            session,
            &TranscriptRecord::QuestionAsked {
                id: id.clone(),
                question,
            },
        )?;
        self.change_session(session.clone(), |asked| asked.question = Some(id))?;
        Ok(Ok(QUESTION_RECORDED.into()))
    }

    /// Closes the session's open question with `outcome` in its history, clears it from the
    /// session and drops the session's `child waits` prompt still queued for its parent; a
    /// session without an open question is left alone.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when the session does not exist.
    pub(super) fn close_question(
        &mut self,
        session: &SessionId,
        outcome: QuestionOutcome,
        decider: Decider,
    ) -> Result<(), AppError> {
        let Some(id) = self.session(session)?.question.clone() else {
            return Ok(());
        };
        self.apply_update(
            session,
            &TranscriptRecord::QuestionClosed {
                id,
                outcome,
                decider,
            },
        )?;
        self.change_session(session.clone(), |asked| asked.question = None)?;
        self.drop_waits(session);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
