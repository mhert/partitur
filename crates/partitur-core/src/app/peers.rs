//! The handlers of the tools that look at the other sessions of the caller's project.
//!
//! `propose_session` lets a session that is neither an orchestrator nor a child propose one
//! new session of its project. The proposal is recorded as a split of one child is, and the
//! user approves or rejects it on the same card; the approved session starts at the top level
//! of the tree.
//!
//! `read_session` answers from memory when the session's transcript is loaded. Otherwise it
//! asks the [`TranscriptReader`](crate::reader::TranscriptReader) for the stored updates and
//! answers when they arrive, off the update loop; such a read does not load the transcript
//! into the app, so the session still counts as not opened. A session deleted before its read
//! is answered is refused as one of another project is.

use std::sync::{Arc, PoisonError};

use super::split::checked_children;
use super::titles::clean_title;
use super::{App, AppError};
use crate::backend::ProposedChild;
use crate::ids::{RepositoryId, SessionId};
use crate::orchestrator::{PROPOSAL_RECORDED, orchestrated_repositories};
use crate::peers::{not_in_project, pages, read_page, session_line};
use crate::session::{Scope, TitleSource};
use crate::tool_call::{PeerTool, ToolAnswer, ToolReply};
use crate::transcript::Transcript;
use crate::workflow;

/// The error result of `propose_session` with a blank prompt.
const NO_PROMPT: &str = "propose_session needs a prompt";

/// The error result of `propose_session` without a repository from a session of a project
/// that has none.
const NO_REPOSITORY: &str = "this project has no repository to start a session in";

impl App {
    /// Answers a peer tool called by `caller`: `list_sessions` and `propose_session` at once,
    /// `read_session` at once or, for a transcript not in memory, later through `reply`, when
    /// this returns `None`.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when `caller` does not exist.
    pub(super) fn peer_tool(
        &mut self,
        caller: &SessionId,
        tool: PeerTool,
        reply: &Arc<dyn ToolReply>,
    ) -> Result<Option<ToolAnswer>, AppError> {
        match tool {
            PeerTool::ListSessions => self.list_sessions(caller).map(|list| Some(Ok(list))),
            PeerTool::ProposeSession {
                repository,
                title,
                prompt,
                base,
            } => self
                .propose_session(caller, repository, &title, prompt, base)
                .map(Some),
            PeerTool::ReadSession { session, page } => {
                self.read_session(caller, &session, page, reply)
            }
        }
    }

    /// `read_session`: `page` of the transcript of `target`, the last one when `None`, if
    /// `target` is a session of the caller's project. A transcript in memory is answered at
    /// once; any other is read through the reader and answered through `reply` once it
    /// arrives, and `None` is returned. A read that fails is answered with the failure's text.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when `caller` does not exist.
    fn read_session(
        &self,
        caller: &SessionId,
        target: &SessionId,
        page: Option<usize>,
        reply: &Arc<dyn ToolReply>,
    ) -> Result<Option<ToolAnswer>, AppError> {
        let project = self.session(caller)?.project.clone();
        let Some(read) = self
            .sessions
            .iter()
            .find(|session| &session.id == target && session.project == project)
        else {
            return Ok(Some(Err(not_in_project(target))));
        };
        let title = read.title.clone();
        if let Some(transcript) = self.transcript(target) {
            return Ok(Some(read_page(
                target,
                &title,
                &pages(transcript.entries()),
                page,
            )));
        }
        let refused = Arc::clone(&self.read_refused);
        let reply = Arc::clone(reply);
        let id = target.clone();
        self.reader.read(
            project,
            target.clone(),
            Box::new(move |updates| {
                let deleted = refused
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .contains(&id);
                let answer = match updates {
                    _ if deleted => Err(not_in_project(&id)),
                    Ok(updates) => {
                        let transcript = Transcript::from_updates(&updates);
                        read_page(&id, &title, &pages(transcript.entries()), page)
                    }
                    Err(error) => Err(error.to_string()),
                };
                reply.send(answer);
            }),
        );
        Ok(None)
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
    /// Returns [`AppError::UnknownSession`] or [`AppError::UnknownProject`] when the caller or its
    /// project does not exist.
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
