//! Bookkeeping for a session's open permission requests.
//!
//! The agent asks for permission inside the connection's dispatch loop, which must not wait
//! for the user. The request's responder is parked here under a fresh [`PermissionId`] and
//! answered later, when the user's decision arrives as a command. Nothing here performs I/O
//! beyond handing the decision to the parked responder.

use std::collections::HashMap;

use partitur_core::{PermissionId, PermissionOptionId};
use thiserror::Error;

/// Sends the user's decision on one permission request back to the agent.
pub(crate) trait Answer {
    /// Answers with the chosen option, or cancels the request with `None`.
    fn answer(self, option: Option<PermissionOptionId>)
    -> Result<(), agent_client_protocol::Error>;
}

/// Why a permission answer could not be delivered.
#[derive(Debug, Error)]
pub(crate) enum RespondError {
    /// No request with this id is open: it was never asked, or was already answered.
    #[error("no open permission request {0}")]
    UnknownPermission(PermissionId),
    /// The agent connection refused the answer.
    #[error("failed to answer permission request: {0}")]
    Answer(#[source] agent_client_protocol::Error),
}

/// The permission requests of one session that wait for the user, by id.
#[derive(Debug)]
pub(crate) struct PendingPermissions<R> {
    next: u64,
    open: HashMap<PermissionId, R>,
}

impl<R> Default for PendingPermissions<R> {
    fn default() -> Self {
        Self {
            next: 0,
            open: HashMap::new(),
        }
    }
}

impl<R: Answer> PendingPermissions<R> {
    /// Parks `responder` and returns the id that answers it. Ids are never reused within a
    /// session.
    pub(crate) fn open(&mut self, responder: R) -> PermissionId {
        let id = PermissionId(self.next);
        self.next += 1;
        self.open.insert(id, responder);
        id
    }

    /// Answers the request `id` with `option`, or cancels it with `None`.
    ///
    /// # Errors
    ///
    /// Returns [`RespondError::UnknownPermission`] when no request `id` is open, and
    /// [`RespondError::Answer`] when the connection refuses the answer.
    pub(crate) fn respond(
        &mut self,
        id: PermissionId,
        option: Option<PermissionOptionId>,
    ) -> Result<(), RespondError> {
        let responder = self
            .open
            .remove(&id)
            .ok_or(RespondError::UnknownPermission(id))?;
        responder.answer(option).map_err(RespondError::Answer)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Records the answer it was given, or fails like a closed connection.
    struct Recorder {
        answers: Arc<Mutex<Vec<Option<PermissionOptionId>>>>,
        fail: bool,
    }

    impl Answer for Recorder {
        fn answer(
            self,
            option: Option<PermissionOptionId>,
        ) -> Result<(), agent_client_protocol::Error> {
            if self.fail {
                return Err(agent_client_protocol::Error::internal_error());
            }
            self.answers.lock().unwrap().push(option);
            Ok(())
        }
    }

    fn recorder(answers: &Arc<Mutex<Vec<Option<PermissionOptionId>>>>) -> Recorder {
        Recorder {
            answers: Arc::clone(answers),
            fail: false,
        }
    }

    #[test]
    fn a_permission_id_is_unique_per_session() {
        let answers = Arc::default();
        let mut pending = PendingPermissions::default();
        let first = pending.open(recorder(&answers));
        let second = pending.open(recorder(&answers));
        pending.respond(first, None).unwrap();
        let third = pending.open(recorder(&answers));
        let ids: HashSet<_> = [first, second, third].into_iter().collect();
        assert_eq!(ids.len(), 3, "ids {first}, {second}, {third} repeat");
    }

    #[test]
    fn responding_to_an_unknown_permission_is_an_error_not_a_panic() {
        let answers = Arc::default();
        let mut pending = PendingPermissions::default();
        let error = pending.respond(PermissionId(42), None).unwrap_err();
        assert!(matches!(
            error,
            RespondError::UnknownPermission(PermissionId(42))
        ));
        assert_eq!(error.to_string(), "no open permission request 42");

        let id = pending.open(recorder(&answers));
        pending.respond(id, None).unwrap();
        let late = pending.respond(id, None).unwrap_err();
        assert!(matches!(late, RespondError::UnknownPermission(late) if late == id));
    }

    #[test]
    fn the_answer_reaches_the_parked_responder() {
        let answers = Arc::default();
        let mut pending = PendingPermissions::default();
        let allowed = pending.open(recorder(&answers));
        let cancelled = pending.open(recorder(&answers));
        pending
            .respond(allowed, Some(PermissionOptionId::new("allow-once")))
            .unwrap();
        pending.respond(cancelled, None).unwrap();
        assert_eq!(
            *answers.lock().unwrap(),
            [Some(PermissionOptionId::new("allow-once")), None]
        );
    }

    #[test]
    fn a_refused_answer_is_reported_and_settles_the_request() {
        let answers = Arc::default();
        let mut pending = PendingPermissions::default();
        let id = pending.open(Recorder {
            answers: Arc::clone(&answers),
            fail: true,
        });
        let error = pending.respond(id, None).unwrap_err();
        assert!(matches!(error, RespondError::Answer(_)));
        assert!(
            error
                .to_string()
                .starts_with("failed to answer permission request: ")
        );
        assert!(matches!(
            pending.respond(id, None),
            Err(RespondError::UnknownPermission(_))
        ));
    }
}
