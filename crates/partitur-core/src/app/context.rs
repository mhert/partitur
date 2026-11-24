//! Reading a session's context, the files its first prompt and its Env tab draw on, in the
//! background.
//!
//! The context of a session is read through the files port, in one request of
//! [`FilesQuery::Read`](crate::files::FilesQuery::Read)s, when its backend session attaches
//! unless a read is on its way already, each time the Env tab is shown for it, and whenever a
//! prompt that carries the instructions is sent, so that prompt takes the files as they are
//! then. Only the latest read's answer is taken; one for a session deleted meanwhile is
//! dropped.
//!
//! A prompt that carries the instructions, the first a backend session gets, waits among the
//! session's queued prompts until the context is read, as it waits while the transcript
//! loads; when the read arrives, the waiting prompts go out once the session is attached and
//! no model change waits for the agent. Until a session's context first arrives, its Env tab
//! lists no instruction files.

use super::{App, AppError};
use crate::context::{SessionContext, Wanted, context_from, context_queries, context_reads};
use crate::files::{FilesAnswer, FilesError, FilesRequestId};
use crate::ids::SessionId;

/// A read of a session's context on its way.
#[derive(Debug)]
pub(super) struct ContextRead {
    /// The request; only its answer is taken.
    request: FilesRequestId,
    /// The files it reads, each with what it is read for, in query order.
    reads: Vec<(std::path::PathBuf, Wanted)>,
}

impl App {
    /// Asks the files port for the context of `session`, as the module describes; a read on
    /// its way already is superseded.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists and
    /// [`AppError::UnknownProject`] when its project is gone.
    pub(super) fn read_context(&mut self, session: &SessionId) -> Result<(), AppError> {
        let found = self.session(session)?;
        let reads = context_reads(self.project(&found.project)?, found);
        let request = self.files_request();
        self.files.query(request, context_queries(&reads));
        self.live_entry(session).context_read = Some(ContextRead { request, reads });
        Ok(())
    }

    /// Reads the context of `session` unless a read is on its way already.
    ///
    /// # Errors
    ///
    /// As [`read_context`](Self::read_context).
    pub(super) fn ensure_context_read(&mut self, session: &SessionId) -> Result<(), AppError> {
        if self
            .live(session)
            .is_some_and(|live| live.context_read.is_some())
        {
            return Ok(());
        }
        self.read_context(session)
    }

    /// Takes the `answers` to `request` when it is the latest read of a session's context:
    /// the context is kept, the prompts that waited for it are sent and the session saved,
    /// and `None` is returned. Any other answer is returned back untouched; one to a superseded read, or to
    /// a read for a session gone since, is then answered to no opening, so it is dropped.
    ///
    /// # Errors
    ///
    /// As [`send_after_load`](Self::send_after_load).
    pub(super) fn context_answered(
        &mut self,
        request: FilesRequestId,
        answers: Vec<Result<FilesAnswer, FilesError>>,
    ) -> Result<Option<Vec<Result<FilesAnswer, FilesError>>>, AppError> {
        let Some((session, live)) = self.live.iter_mut().find(|(_, live)| {
            live.context_read
                .as_ref()
                .is_some_and(|read| read.request == request)
        }) else {
            return Ok(Some(answers));
        };
        let session = session.clone();
        let waiting = !live.queued.is_empty();
        if let Some(read) = live.context_read.take() {
            live.context = Some(context_from(read.reads, answers));
        }
        self.send_after_load(&session)?;
        if waiting {
            // A prompt that went out has the instructions marked as sent.
            self.change_session(session, |_| {})?;
        }
        Ok(None)
    }

    /// Whether a prompt to `session` has to wait for its context: it carries the
    /// instructions, and the context has not arrived or a read is on its way.
    pub(super) fn awaits_context(&self, session: &SessionId) -> bool {
        let injected = self
            .session(session)
            .is_ok_and(|found| found.instructions_injected);
        !injected
            && self
                .live(session)
                .is_none_or(|live| live.context.is_none() || live.context_read.is_some())
    }

    /// The latest context of `session` that arrived, `None` before the first.
    pub(super) fn context_of(&self, session: &SessionId) -> Option<&SessionContext> {
        self.live(session)?.context.as_ref()
    }
}

#[cfg(test)]
mod tests;
