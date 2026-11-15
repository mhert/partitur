//! The tools that change session titles from inside a session.
//!
//! A standalone session or an orchestrator renames itself with `rename_session`, and an
//! orchestrator also renames its own children with it. The new title is trimmed, replaces any
//! earlier one, also one the user typed, and is [chosen](TitleSource::Chosen), so the agent's
//! automatic title never replaces it. A blank title changes nothing and is an error result.
//! Which session may call which tool is checked before the call reaches this module.

use super::{App, AppError};
use crate::ids::SessionId;
use crate::session::TitleSource;
use crate::tool_call::ToolAnswer;

/// The error result of `rename_session` with a blank title.
const NO_RENAME_TITLE: &str = "rename_session needs a title";

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
}
