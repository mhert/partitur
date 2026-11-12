//! The reply of a tool call the HTTP handler waits for.

use std::sync::{Mutex, PoisonError};

use futures::channel::oneshot;
use partitur_core::{ToolAnswer, ToolReply};

/// A [`ToolReply`] that hands the answer to the HTTP handler waiting on its receiver.
#[derive(Debug)]
pub(crate) struct OneshotReply {
    /// The sending half, until the answer went through it.
    sender: Mutex<Option<oneshot::Sender<ToolAnswer>>>,
}

impl OneshotReply {
    /// A reply and the receiver its answer arrives on.
    pub(crate) fn new() -> (Self, oneshot::Receiver<ToolAnswer>) {
        let (sender, receiver) = oneshot::channel();
        (
            Self {
                sender: Mutex::new(Some(sender)),
            },
            receiver,
        )
    }
}

impl ToolReply for OneshotReply {
    /// Whether the answer is still unsent and its receiver still there.
    fn is_waiting(&self) -> bool {
        self.sender
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|sender| !sender.is_canceled())
    }

    fn send(&self, answer: ToolAnswer) {
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(sender) = sender {
            // A receiver that is gone stopped waiting; the answer has nowhere to go.
            let _ = sender.send(answer);
        }
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

    use super::*;

    #[test]
    fn a_reply_waits_until_its_receiver_is_gone() {
        let (reply, answer) = OneshotReply::new();
        assert!(reply.is_waiting());

        drop(answer);

        assert!(!reply.is_waiting());
        reply.send(Ok("too late".into()));
    }

    #[test]
    fn a_reply_delivers_only_its_first_answer() {
        let (reply, mut answer) = OneshotReply::new();

        reply.send(Ok("first".into()));
        reply.send(Err("second".into()));

        assert_eq!(answer.try_recv().unwrap(), Some(Ok("first".to_owned())));
        assert!(!reply.is_waiting());
    }
}
