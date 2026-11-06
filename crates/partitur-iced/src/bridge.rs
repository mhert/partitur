//! Carries what the backend reports into the iced runtime.

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use iced::Task;
use partitur_core::BackendEvent;

use crate::app::Message;

/// A task that turns every event `receiver` yields into a [`Message::Backend`], in order,
/// until every sender is gone.
pub fn events(receiver: UnboundedReceiver<BackendEvent>) -> Task<Message> {
    Task::stream(receiver.map(Message::Backend))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use futures::channel::mpsc;
    use futures::future;
    use iced_test::futures::futures::executor::block_on;
    use iced_test::runtime::Action;
    use iced_test::runtime::task::into_stream;
    use partitur_core::{SessionId, Update};

    use super::*;

    fn notice(text: &str) -> BackendEvent {
        BackendEvent::Update {
            session: SessionId::new(),
            update: Update::Notice(text.into()),
        }
    }

    #[test]
    fn every_backend_event_becomes_a_message_in_order() {
        let (sender, receiver) = mpsc::unbounded();
        let (first, second) = (notice("one"), notice("two"));
        sender.unbounded_send(first.clone()).unwrap();
        sender.unbounded_send(second.clone()).unwrap();
        drop(sender);

        let actions = into_stream(events(receiver)).unwrap();
        let messages: Vec<Message> = block_on(
            actions
                .filter_map(|action| {
                    future::ready(match action {
                        Action::Output(message) => Some(message),
                        _ => None,
                    })
                })
                .collect(),
        );

        assert_eq!(
            messages,
            [Message::Backend(first), Message::Backend(second)]
        );
    }
}
