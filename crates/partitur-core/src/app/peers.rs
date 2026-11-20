//! The handlers of the tools that look at the other sessions of the caller's project.

use std::sync::Arc;

use super::{App, AppError};
use crate::ids::SessionId;
use crate::peers::session_line;
use crate::tool_call::{PeerTool, ToolReply};
use crate::workflow;

/// What a peer tool that is not built yet answers.
const NOT_AVAILABLE: &str = "not available yet";

impl App {
    /// Answers a peer tool called by `caller`: `list_sessions` at once, the others not yet.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when `caller` does not exist.
    pub(super) fn peer_tool(
        &mut self,
        caller: &SessionId,
        tool: &PeerTool,
        _reply: &Arc<dyn ToolReply>,
    ) -> Result<Result<String, String>, AppError> {
        match tool {
            PeerTool::ListSessions => self.list_sessions(caller).map(Ok),
            PeerTool::ProposeSession { .. } | PeerTool::ReadSession { .. } => {
                Ok(Err(NOT_AVAILABLE.to_owned()))
            }
        }
    }

    /// `list_sessions`: the caller's project's sessions under `sessions:`, one line each: the
    /// live ones newest first, then the archived ones newest first.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when `caller` does not exist.
    pub(super) fn list_sessions(&self, caller: &SessionId) -> Result<String, AppError> {
        let project = self.session(caller)?.project.clone();
        let mut sessions: Vec<_> = self
            .sessions
            .iter()
            .filter(|session| session.project == project)
            .collect();
        sessions.sort_by_key(|session| {
            (
                session.archived.is_some(),
                std::cmp::Reverse(session.created_at),
            )
        });
        let lines: Vec<String> = sessions
            .into_iter()
            .map(|session| {
                let gate = self
                    .gate_open(&session.id)
                    .map(|(module, kind)| workflow::gate_texts(module, &kind).word);
                session_line(session, caller, gate)
            })
            .collect();
        Ok(format!("sessions:\n{}", lines.join("\n")))
    }
}

#[cfg(test)]
mod tests;
