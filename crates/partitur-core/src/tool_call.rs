//! Tool calls from sessions as an incoming port of the core.

use std::fmt;
use std::sync::Arc;

use crate::backend::OrchestratorTool;
use crate::drawing::Drawing;
use crate::ids::{RepositoryId, SessionId};
use crate::question::{QUESTION_INSTRUCTIONS, Question};
use crate::workflow::{self, ModuleEvent};

/// The answer to a tool call: `Ok` text is the tool's result, `Err` text an error result.
pub type ToolAnswer = Result<String, String>;

/// Where the answer to a [`ToolCall`] goes.
pub trait ToolReply: Send + Sync + fmt::Debug {
    /// Whether the caller still waits for an answer; `false` once it stopped waiting.
    fn is_waiting(&self) -> bool;

    /// Delivers the answer. Delivers at most once: later calls are ignored.
    fn send(&self, answer: ToolAnswer);
}

/// Which of Partitur's tools a session is offered, by the part it plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRole {
    /// A session that is neither an orchestrator nor a child.
    Plain,
    /// A session that orchestrates child sessions.
    Orchestrator,
    /// A child session of an orchestrator.
    Child,
}

/// A tool about the calling session itself, or a session in its care.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionTool {
    /// Renames the caller, or `session`, one of the calling orchestrator's children.
    Rename {
        /// The new title, before it is trimmed.
        title: String,
        /// The child to rename; `None` renames the caller.
        session: Option<SessionId>,
    },
    /// Proposes a new title for the calling child to its orchestrator, which decides.
    ProposeTitle {
        /// The proposed title, before it is trimmed.
        title: String,
    },
}

impl From<SessionTool> for PartiturTool {
    fn from(tool: SessionTool) -> Self {
        Self::Session(tool)
    }
}

/// A tool about the other sessions of the caller's project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerTool {
    /// Proposes one new top-level session in the caller's project.
    ProposeSession {
        /// The repository the session works in; `None` for the caller's own.
        repository: Option<RepositoryId>,
        /// The session's display title.
        title: String,
        /// The first prompt the session receives.
        prompt: String,
        /// The branch a new branch starts from, as the caller suggests it.
        base: Option<String>,
    },
    /// Lists the sessions of the caller's project.
    ListSessions,
    /// Reads a session's conversation as text, a page at a time.
    ReadSession {
        /// The session to read.
        session: SessionId,
        /// The page to read; `None` for the last one.
        page: Option<usize>,
    },
}

impl From<PeerTool> for PartiturTool {
    fn from(tool: PeerTool) -> Self {
        Self::Peers(tool)
    }
}

/// One of Partitur's tools, asked for by a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartiturTool {
    /// A tool only an orchestrator may call.
    Orchestrator(OrchestratorTool),
    /// A report about the session's workflow, which any session may make.
    Workflow(ModuleEvent),
    /// A question for the user with options to pick from, which any session may ask.
    AskUser(Question),
    /// A tool about the calling session's title, offered by its role.
    Session(SessionTool),
    /// A tool about the other sessions of the project.
    Peers(PeerTool),
    /// A drawing to show the user, which any session may make.
    Show(Drawing),
}

/// The block appended to every session's system prompt: the question instructions, then the
/// workflow instructions, joined by a line break.
#[must_use]
pub fn tool_instructions() -> String {
    format!("{QUESTION_INSTRUCTIONS}\n{}", workflow::instructions())
}

impl From<OrchestratorTool> for PartiturTool {
    fn from(tool: OrchestratorTool) -> Self {
        Self::Orchestrator(tool)
    }
}

impl From<ModuleEvent> for PartiturTool {
    fn from(event: ModuleEvent) -> Self {
        Self::Workflow(event)
    }
}

/// A session calling one of Partitur's tools, with the way to answer it.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// The session making the call.
    pub session: SessionId,
    /// What it asks for.
    pub tool: PartiturTool,
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
                decision: None,
            }
            .into(),
            reply: reply.clone(),
        }
    }

    #[test]
    fn tool_instructions_start_with_the_question_block() {
        let instructions = tool_instructions();

        assert!(instructions.starts_with("<partitur-question>"));
        assert!(instructions.contains(&workflow::instructions()));
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
