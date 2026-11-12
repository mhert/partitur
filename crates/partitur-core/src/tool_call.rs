//! Tool calls from orchestrator sessions as an incoming port of the core.

use std::fmt;
use std::sync::Arc;

use crate::backend::OrchestratorTool;
use crate::ids::SessionId;

/// The answer to a tool call: `Ok` text is the tool's result, `Err` text an error result.
pub type ToolAnswer = Result<String, String>;

/// Where the answer to a [`ToolCall`] goes.
pub trait ToolReply: Send + Sync + fmt::Debug {
    /// Whether the caller still waits for an answer; `false` once it stopped waiting.
    fn is_waiting(&self) -> bool;

    /// Delivers the answer. Delivers at most once: later calls are ignored.
    fn send(&self, answer: ToolAnswer);
}

/// An orchestrator session calling one of Partitur's tools, with the way to answer it.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// The session making the call.
    pub session: SessionId,
    /// What it asks for.
    pub tool: OrchestratorTool,
    /// Where the answer goes.
    pub reply: Arc<dyn ToolReply>,
}

impl PartialEq for ToolCall {
    /// Two calls are equal when they come from the same session, ask for the same thing and
    /// share one reply.
    fn eq(&self, other: &Self) -> bool {
        self.session == other.session
            && self.tool == other.tool
            && Arc::ptr_eq(&self.reply, &other.reply)
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
    use crate::testing::RecordingReply;

    fn call(session: &SessionId, prompt: &str, reply: &Arc<RecordingReply>) -> ToolCall {
        ToolCall {
            session: session.clone(),
            tool: OrchestratorTool::SendToChild {
                child: session.clone(),
                prompt: prompt.into(),
            },
            reply: reply.clone(),
        }
    }

    #[test]
    fn calls_are_equal_when_session_tool_and_reply_match() {
        let reply = RecordingReply::new();
        let a = SessionId::new();

        assert_eq!(call(&a, "go", &reply), call(&a, "go", &reply));
        assert_eq!(call(&a, "go", &reply), call(&a, "go", &reply).clone());
    }

    #[test]
    fn calls_differ_in_session_tool_or_reply() {
        let reply = RecordingReply::new();
        let a = SessionId::new();

        assert_ne!(
            call(&a, "go", &reply),
            call(&SessionId::new(), "go", &reply)
        );
        assert_ne!(call(&a, "go", &reply), call(&a, "stop", &reply));
        assert_ne!(
            call(&a, "go", &reply),
            call(&a, "go", &RecordingReply::new())
        );
    }

    #[test]
    fn a_reply_keeps_only_its_first_answer() {
        let reply = RecordingReply::new();
        assert!(reply.is_waiting());

        reply.send(Ok("first".into()));
        reply.send(Err("second".into()));

        assert_eq!(reply.answers(), [Ok("first".to_owned())]);
    }
}
