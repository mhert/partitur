//! The handlers of the tools that look at the other sessions of the caller's project.
//!
//! `propose_session` lets a session that is neither an orchestrator nor a child propose one
//! new session of its project. The proposal is recorded as a split of one child is, and the
//! user approves or rejects it on the same card; the approved session starts at the top level
//! of the tree.

use std::sync::Arc;

use super::orchestration::checked_children;
use super::titles::clean_title;
use super::{App, AppError};
use crate::backend::ProposedChild;
use crate::ids::{RepositoryId, SessionId};
use crate::orchestrator::{PROPOSAL_RECORDED, orchestrated_repositories};
use crate::peers::session_line;
use crate::session::{Scope, TitleSource};
use crate::tool_call::{PeerTool, ToolAnswer, ToolReply};
use crate::workflow;

/// What a peer tool that is not built yet answers.
const NOT_AVAILABLE: &str = "not available yet";

/// The error result of `propose_session` with a blank prompt.
const NO_PROMPT: &str = "propose_session needs a prompt";

/// The error result of `propose_session` without a repository from a session of a project
/// that has none.
const NO_REPOSITORY: &str = "this project has no repository to start a session in";

impl App {
    /// Answers a peer tool called by `caller`: `list_sessions` and `propose_session` at once,
    /// `read_session` not yet.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when `caller` does not exist and
    /// [`AppError::Store`] when a proposal cannot be saved.
    pub(super) fn peer_tool(
        &mut self,
        caller: &SessionId,
        tool: PeerTool,
        _reply: &Arc<dyn ToolReply>,
    ) -> Result<ToolAnswer, AppError> {
        match tool {
            PeerTool::ListSessions => self.list_sessions(caller).map(Ok),
            PeerTool::ProposeSession {
                repository,
                title,
                prompt,
                base,
            } => self.propose_session(caller, repository, &title, prompt, base),
            PeerTool::ReadSession { .. } => Ok(Err(NOT_AVAILABLE.to_owned())),
        }
    }

    /// `propose_session`: records one proposed session in `repository`, the caller's own when
    /// it is `None` (for a session of the whole project, the repository an orchestrator
    /// would work on), with the cleaned `title`, `prompt` and suggested `base`, and answers at
    /// once as `propose_children` does. A blank title is derived from the prompt.
    ///
    /// The call is refused, and nothing recorded, when the prompt is blank, the repository is
    /// not one of the project's, or the caller names none and the project has none.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] or [`AppError::UnknownProject`] when the caller
    /// or its project does not exist and [`AppError::Store`] when the proposal cannot be
    /// saved.
    fn propose_session(
        &mut self,
        caller: &SessionId,
        repository: Option<RepositoryId>,
        title: &str,
        prompt: String,
        base: Option<String>,
    ) -> Result<ToolAnswer, AppError> {
        if prompt.trim().is_empty() {
            return Ok(Err(NO_PROMPT.to_owned()));
        }
        let session = self.session(caller)?;
        let project = self.project(&session.project)?;
        let repository = match (repository, &session.scope) {
            (Some(repository), _) => repository,
            (None, Scope::Repository(repository)) => repository.clone(),
            (None, Scope::Project) => match orchestrated_repositories(project).first() {
                Some(repository) => repository.id.clone(),
                None => return Ok(Err(NO_REPOSITORY.to_owned())),
            },
        };
        let child = ProposedChild {
            repository,
            prompt,
            title: clean_title(title),
            title_source: TitleSource::Given,
            base,
        };
        let children = match checked_children(project, vec![child]) {
            Ok(children) => children,
            Err(refusal) => return Ok(Err(refusal)),
        };
        self.record_proposal(caller, children)?;
        Ok(Ok(PROPOSAL_RECORDED.to_owned()))
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
