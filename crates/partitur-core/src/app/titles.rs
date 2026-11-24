//! The tools that change session titles from inside a session.
//!
//! A standalone session or an orchestrator renames itself with `rename_session`, and an
//! orchestrator also renames its own children with it. The new title is cleaned, replaces any
//! earlier one, also one the user typed, and is [chosen](TitleSource::Chosen), so the agent's
//! automatic title never replaces it. A child instead proposes a title with
//! `propose_title`: its title stays, and the cleaned proposal reaches its orchestrator as a
//! generated prompt, delivered like a child result; a newer proposal replaces one that still
//! waits. A child learns its title from a note before its first prompt. Cleaning puts a title on one line, with every run of whitespace one space,
//! and cuts it like a title derived from a prompt; a title that is blank then changes nothing
//! and is an error result. Which session may call which tool is checked before the call reaches
//! this module.

use super::scheduler::Generated;
use super::{App, AppError};
use crate::backend::TitleProposal;
use crate::ids::SessionId;
use crate::peers::one_line;
use crate::session::{Scope, TitleSource, title_from_prompt};
use crate::tool_call::ToolAnswer;

/// The error result of `rename_session` with a blank title.
const NO_RENAME_TITLE: &str = "rename_session needs a title";

/// The error result of `propose_title` with a blank title.
const NO_PROPOSED_TITLE: &str = "propose_title needs a title";

/// The error result of `propose_title` from a child that works in no repository.
const NO_REPOSITORY: &str = "this session works in no repository";

/// The answer to a child's `propose_title` call.
const PROPOSAL_SENT: &str = "sent to the orchestrator";

/// The note a child's first prompt starts with: its title, and when to propose another.
pub(super) fn child_title_note(title: &str) -> String {
    format!(
        "Your session is titled \"{title}\". If your work drifts from that title, propose a new one with propose_title."
    )
}

/// `title` on one line, every run of whitespace one space, cut like a title derived from a
/// prompt.
pub(super) fn clean_title(title: &str) -> String {
    title_from_prompt(&one_line(title))
}

impl App {
    /// `rename_session`: gives `target`, one of the children of the orchestrator `caller`,
    /// or `caller` itself when `target` is `None` or names it, the cleaned `title`, and
    /// answers with the old title and the new one.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when the session to rename does not exist.
    pub(super) fn rename_by_agent(
        &mut self,
        caller: &SessionId,
        target: Option<&SessionId>,
        title: &str,
    ) -> Result<ToolAnswer, AppError> {
        let title = clean_title(title);
        if title.is_empty() {
            return Ok(Err(NO_RENAME_TITLE.to_owned()));
        }
        let renamed = match target.filter(|target| *target != caller) {
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
            session.title.clone_from(&title);
            session.title_source = TitleSource::Chosen;
        })?;
        Ok(Ok(format!("renamed \"{old}\" to \"{title}\"")))
    }

    /// `propose_title`: brings the cleaned `title` the child `child` proposes for itself to
    /// its orchestrator, with the child's current title and whether the user typed it, and
    /// answers that it was sent. While the orchestrator is busy the proposal waits, replacing
    /// one the child made before, and takes the child's title as it is when it goes out. A
    /// child that works in no repository proposes nothing and gets an error result; the
    /// caller's role is checked before.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when `child` or its parent does not exist,
    /// [`AppError::Store`] when the parent's transcript cannot be loaded and [`AppError::Backend`]
    /// when the backend cannot queue the prompt.
    pub(super) fn propose_title(
        &mut self,
        child: &SessionId,
        title: &str,
    ) -> Result<ToolAnswer, AppError> {
        let proposed = clean_title(title);
        if proposed.is_empty() {
            return Ok(Err(NO_PROPOSED_TITLE.to_owned()));
        }
        let session = self.session(child)?;
        let (Some(parent), Scope::Repository(repository)) = (session.role.parent(), &session.scope)
        else {
            return Ok(Err(NO_REPOSITORY.to_owned()));
        };
        let parent = parent.clone();
        let proposal = Generated::TitleProposal(TitleProposal {
            child: child.clone(),
            repository: repository.clone(),
            title: session.title.clone(),
            proposed,
            by_user: session.title_source == TitleSource::Given,
        });
        self.ensure_transcript(&parent)?;
        self.deliver(&parent, proposal)?;
        Ok(Ok(PROPOSAL_SENT.to_owned()))
    }
}
