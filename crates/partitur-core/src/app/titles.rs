//! The tools that change session titles from inside a session.
//!
//! A standalone session or an orchestrator renames itself with `rename_session`, and an
//! orchestrator also renames its own children with it. The new title is trimmed, replaces any
//! earlier one, also one the user typed, and is [chosen](TitleSource::Chosen), so the agent's
//! automatic title never replaces it. A child instead proposes a title with
//! `propose_title`: its title stays, and the trimmed proposal reaches its orchestrator as a
//! generated prompt, delivered like a child result. A blank title changes nothing and is an
//! error result. Which session may call which tool is checked before the call reaches this
//! module.

use super::orchestration::Generated;
use super::{App, AppError};
use crate::backend::TitleProposal;
use crate::ids::SessionId;
use crate::session::{Scope, TitleSource};
use crate::tool_call::ToolAnswer;

/// The error result of `rename_session` with a blank title.
const NO_RENAME_TITLE: &str = "rename_session needs a title";

/// The error result of `propose_title` with a blank title.
const NO_PROPOSED_TITLE: &str = "propose_title needs a title";

/// The answer to a child's `propose_title` call.
const PROPOSAL_SENT: &str = "sent to the orchestrator";

impl App {
    /// `rename_session`: gives `target`, one of the children of the orchestrator `caller`,
    /// or `caller` itself when `target` is `None`, the trimmed `title`, and answers with the
    /// old title and the new one.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when the session to rename does not exist and
    /// [`AppError::Store`] when the session list cannot be saved.
    pub(super) fn rename_by_agent(
        &mut self,
        caller: &SessionId,
        target: Option<&SessionId>,
        title: &str,
    ) -> Result<ToolAnswer, AppError> {
        let title = title.trim();
        if title.is_empty() {
            return Ok(Err(NO_RENAME_TITLE.to_owned()));
        }
        let renamed = match target {
            Some(child) => {
                if let Err(refusal) = self.check_child(caller, child) {
                    return Ok(Err(refusal));
                }
                child
            }
            None => caller,
        };
        let old = self.session(renamed)?.title.clone();
        self.change_session(renamed.clone(), |session| {
            title.clone_into(&mut session.title);
            session.title_source = TitleSource::Chosen;
        })?;
        Ok(Ok(format!("renamed \"{old}\" to \"{title}\"")))
    }

    /// `propose_title`: brings the trimmed `title` the child `child` proposes for itself to
    /// its orchestrator, with the child's current title and whether the user typed it, and
    /// answers that it was sent. A session without a parent working in a repository proposes
    /// nothing; the caller's role is checked before.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when `child` or its parent does not exist,
    /// [`AppError::Store`] when the parent's transcript cannot be loaded or the change cannot be
    /// saved and [`AppError::Backend`] when the backend cannot queue the prompt.
    pub(super) fn propose_title(
        &mut self,
        child: &SessionId,
        title: &str,
    ) -> Result<ToolAnswer, AppError> {
        let proposed = title.trim();
        if proposed.is_empty() {
            return Ok(Err(NO_PROPOSED_TITLE.to_owned()));
        }
        let session = self.session(child)?;
        let (Some(parent), Scope::Repository(repository)) = (&session.parent, &session.scope)
        else {
            return Err(AppError::UnknownSession(child.clone()));
        };
        let parent = parent.clone();
        let proposal = Generated::TitleProposal(TitleProposal {
            child: child.clone(),
            repository: repository.clone(),
            title: session.title.clone(),
            proposed: proposed.to_owned(),
            by_user: session.title_source == TitleSource::Given,
        });
        self.ensure_transcript(&parent)?;
        self.deliver(&parent, proposal)?;
        Ok(Ok(PROPOSAL_SENT.to_owned()))
    }
}
