//! Carries what the backend, the git port, the store's queue, the files port and the widget
//! engine report and the tool calls of orchestrator sessions into the iced runtime.

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use iced::Task;
use partitur_core::widget_host::WidgetHostEvent;
use partitur_core::{BackendEvent, FilesEvent, GitEvent, StoreEvent, ToolCall};

use crate::app::Message;

/// A task that turns every event `receiver` yields into a [`Message::Backend`], in order,
/// until every sender is gone.
pub fn events(receiver: UnboundedReceiver<BackendEvent>) -> Task<Message> {
    Task::stream(receiver.map(Message::Backend))
}

/// A task that turns every event `receiver` yields into a [`Message::Git`], in order, until
/// every sender is gone.
pub fn git_events(receiver: UnboundedReceiver<GitEvent>) -> Task<Message> {
    Task::stream(receiver.map(Message::Git))
}

/// A task that turns every event `receiver` yields into a [`Message::Store`], in order, until
/// every sender is gone.
pub fn store_events(receiver: UnboundedReceiver<StoreEvent>) -> Task<Message> {
    Task::stream(receiver.map(Message::Store))
}

/// A task that turns every event `receiver` yields into a [`Message::Files`], in order, until
/// every sender is gone.
pub fn files_events(receiver: UnboundedReceiver<FilesEvent>) -> Task<Message> {
    Task::stream(receiver.map(Message::Files))
}

/// A task that turns every event `receiver` yields into a [`Message::Widget`], in order, until
/// every sender is gone.
pub fn widget_events(receiver: UnboundedReceiver<WidgetHostEvent>) -> Task<Message> {
    Task::stream(receiver.map(Message::Widget))
}

/// A task that turns every call `receiver` yields into a [`Message::Tool`], in order, until
/// every sender is gone.
pub fn tool_calls(receiver: UnboundedReceiver<ToolCall>) -> Task<Message> {
    Task::stream(receiver.map(Message::Tool))
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
    use partitur_core::testing::RecordingReply;
    use partitur_core::{AgentUpdate, GitEvent, GitRequestId, OrchestratorTool, SessionId};

    use super::*;

    /// The messages `task` outputs, in order.
    fn outputs(task: Task<Message>) -> Vec<Message> {
        let actions = into_stream(task).unwrap();
        block_on(
            actions
                .filter_map(|action| {
                    future::ready(match action {
                        Action::Output(message) => Some(message),
                        _ => None,
                    })
                })
                .collect(),
        )
    }

    fn notice(text: &str) -> BackendEvent {
        BackendEvent::Update {
            session: SessionId::new(),
            update: AgentUpdate::Notice(text.into()),
        }
    }

    #[test]
    fn every_backend_event_becomes_a_message_in_order() {
        let (sender, receiver) = mpsc::unbounded();
        let (first, second) = (notice("one"), notice("two"));
        sender.unbounded_send(first.clone()).unwrap();
        sender.unbounded_send(second.clone()).unwrap();
        drop(sender);

        let messages = outputs(events(receiver));

        assert_eq!(
            messages,
            [Message::Backend(first), Message::Backend(second)]
        );
    }

    #[test]
    fn every_git_event_becomes_a_message_in_order() {
        let (sender, receiver) = mpsc::unbounded();
        let first = GitEvent::Answered {
            request: GitRequestId(1),
            answers: Vec::new(),
        };
        let second = GitEvent::Ran {
            request: GitRequestId(2),
            done: 0,
            result: Ok(()),
        };
        sender.unbounded_send(first.clone()).unwrap();
        sender.unbounded_send(second.clone()).unwrap();
        drop(sender);

        let actions = into_stream(git_events(receiver)).unwrap();
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

        assert_eq!(messages, [Message::Git(first), Message::Git(second)]);
    }

    #[test]
    fn every_store_event_becomes_a_message_in_order() {
        let (sender, receiver) = mpsc::unbounded();
        let failed = |text: &str| {
            StoreEvent::failed(
                &partitur_core::StoreCommand::SaveProjects(Vec::new()),
                partitur_core::StoreError::Unavailable(text.into()),
            )
        };
        let (first, second) = (failed("one"), failed("two"));
        sender.unbounded_send(first.clone()).unwrap();
        sender.unbounded_send(second.clone()).unwrap();
        drop(sender);

        let messages = outputs(store_events(receiver));

        assert_eq!(messages, [Message::Store(first), Message::Store(second)]);
    }

    #[test]
    fn every_files_event_becomes_a_message_in_order() {
        let (sender, receiver) = mpsc::unbounded();
        let answered = |number| partitur_core::FilesEvent::Answered {
            request: partitur_core::FilesRequestId(number),
            answers: Vec::new(),
        };
        let (first, second) = (answered(1), answered(2));
        sender.unbounded_send(first.clone()).unwrap();
        sender.unbounded_send(second.clone()).unwrap();
        drop(sender);

        let messages = outputs(files_events(receiver));

        assert_eq!(messages, [Message::Files(first), Message::Files(second)]);
    }

    #[test]
    fn every_widget_event_becomes_a_message_in_order() {
        let (sender, receiver) = mpsc::unbounded();
        let failed = |error: &str| WidgetHostEvent::Failed {
            view: partitur_core::widget_host::ViewId(1),
            error: error.into(),
        };
        let (first, second) = (failed("one"), failed("two"));
        sender.unbounded_send(first.clone()).unwrap();
        sender.unbounded_send(second.clone()).unwrap();
        drop(sender);

        let messages = outputs(widget_events(receiver));

        assert_eq!(messages, [Message::Widget(first), Message::Widget(second)]);
    }

    fn sending(prompt: &str) -> ToolCall {
        let session = SessionId::new();
        ToolCall {
            session: session.clone(),
            tool: OrchestratorTool::SendToChild {
                child: session,
                prompt: prompt.into(),
                decision: None,
            }
            .into(),
            reply: RecordingReply::new(),
        }
    }

    #[test]
    fn every_tool_call_becomes_a_message_in_order() {
        let (sender, receiver) = mpsc::unbounded();
        let (first, second) = (sending("one"), sending("two"));
        sender.unbounded_send(first.clone()).unwrap();
        sender.unbounded_send(second.clone()).unwrap();
        drop(sender);

        let messages = outputs(tool_calls(receiver));

        assert_eq!(messages, [Message::Tool(first), Message::Tool(second)]);
    }
}
