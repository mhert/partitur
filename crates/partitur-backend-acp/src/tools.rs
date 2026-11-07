//! Bookkeeping for the orchestrator tool calls that wait for the core's answer.
//!
//! A tool call an orchestrator makes is parked here under a fresh [`ToolRequestId`] while
//! the core decides it; the core's answer arrives later through
//! [`Backend::answer_tool`](partitur_core::Backend::answer_tool) and is handed to the parked
//! call's receiver. Nothing here performs I/O.
//!
//! Every clone shares the same parked calls and the same id counter, so an id is never
//! reused while the backend runs.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use partitur_core::{BackendError, ToolRequestId};
use tokio::sync::oneshot;

/// The core's answer to a tool call: `Ok` text is the tool's result, `Err` text an error
/// result.
pub(crate) type ToolAnswer = Result<String, String>;

/// The parked calls, by id, each with the sender that delivers its answer.
type Parked = HashMap<ToolRequestId, oneshot::Sender<ToolAnswer>>;

/// The tool calls that wait for the core's answer, shared by the backend and its runtime.
#[derive(Debug, Clone, Default)]
pub(crate) struct PendingTools {
    ids: Arc<AtomicU64>,
    parked: Arc<Mutex<Parked>>,
}

impl PendingTools {
    /// Parks a new call and returns its id and the receiver its answer arrives on. Ids are
    /// never reused by any clone.
    pub(crate) fn open(&self) -> (ToolRequestId, oneshot::Receiver<ToolAnswer>) {
        let id = ToolRequestId(self.ids.fetch_add(1, Ordering::Relaxed));
        let (sender, receiver) = oneshot::channel();
        self.lock().insert(id, sender);
        (id, receiver)
    }

    /// Hands `result` to the call `call` and settles it. A call whose receiver is gone (it
    /// stopped waiting) still counts as answered.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::UnknownToolCall`] when no call `call` waits: it was never
    /// opened or was already answered.
    pub(crate) fn answer(
        &self,
        call: ToolRequestId,
        result: ToolAnswer,
    ) -> Result<(), BackendError> {
        let sender = self
            .lock()
            .remove(&call)
            .ok_or(BackendError::UnknownToolCall(call))?;
        // A dropped receiver gave up waiting; there is nobody left to tell.
        let _ = sender.send(result);
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, Parked> {
        self.parked.lock().unwrap_or_else(PoisonError::into_inner)
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

    use super::*;

    #[test]
    fn an_opened_call_is_answered_through_its_receiver() {
        let tools = PendingTools::default();
        let (call, mut receiver) = tools.open();

        tools.answer(call, Ok("x".into())).unwrap();

        // An answer is there at once; a test that waited for it would hang without one.
        assert_eq!(receiver.try_recv(), Ok(Ok("x".into())));
    }

    #[test]
    fn answering_an_unknown_or_already_answered_call_is_an_error() {
        let tools = PendingTools::default();
        assert_eq!(
            tools.answer(ToolRequestId(42), Ok("x".into())),
            Err(BackendError::UnknownToolCall(ToolRequestId(42)))
        );

        let (call, _receiver) = tools.open();
        tools.answer(call, Err("no".into())).unwrap();
        assert_eq!(
            tools.answer(call, Ok("again".into())),
            Err(BackendError::UnknownToolCall(call))
        );
    }

    #[test]
    fn ids_never_repeat_across_clones() {
        let tools = PendingTools::default();
        let clone = tools.clone();

        let (first, _a) = tools.open();
        let (second, _b) = clone.open();
        let (third, _c) = tools.open();
        let (fourth, _d) = clone.open();

        let ids: HashSet<_> = [first, second, third, fourth].into_iter().collect();
        assert_eq!(
            ids.len(),
            4,
            "ids {first}, {second}, {third}, {fourth} repeat"
        );
    }

    #[test]
    fn a_dropped_receiver_still_counts_as_answered() {
        let tools = PendingTools::default();
        let (call, receiver) = tools.open();
        drop(receiver);

        assert_eq!(tools.answer(call, Ok("late".into())), Ok(()));
        assert_eq!(
            tools.answer(call, Ok("later".into())),
            Err(BackendError::UnknownToolCall(call))
        );
    }
}
