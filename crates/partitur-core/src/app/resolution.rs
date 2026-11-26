//! Deciding an orchestrator's permission requests on what the files port found about the
//! paths they name.
//!
//! An orchestrator's read or edit that names paths is decided on where those paths really
//! lead: a read inside its children's worktrees is allowed without asking, and an edit inside
//! its workflow folders is not denied. Resolving them would wait on the disk, so the request
//! asks the files port in the background, in one request that resolves every path the
//! decision looks at and reads the workflow modules' configuration files in the working
//! folder, which name BMAD's output folder as it is when the call is made.
//!
//! Until the answer arrives the request waits, as it would for the user, but nothing grants
//! it: an answer that allows it is ignored, and so is turning on auto-allow, which the
//! decision reads when the answer comes. It can still be cancelled, and the end of its turn
//! keeps, cancels or lapses it like any other request. The answer then decides it exactly as
//! the check of the same facts always did: allowed, denied for the orchestrator, or asked. A
//! resolution that failed resolves nothing, so the request is decided as one whose paths
//! cannot be resolved. An answer for a request that was answered, lapsed or whose session was
//! deleted meanwhile is dropped.

use super::{App, AppError, PermissionCall};
use crate::app::live::Resolving;
use crate::backend::PermissionOption;
use crate::files::{FilesAnswer, FilesError, FilesRequestId};
use crate::ids::{PermissionId, SessionId};
use crate::orchestrator::PathCheck;

impl App {
    /// Keeps the request `permission` of `session` for `call`, offering `options`, waiting
    /// while the files port answers what `check` needs, as the [module](self) describes.
    pub(super) fn resolve_paths(
        &mut self,
        session: &SessionId,
        permission: PermissionId,
        call: PermissionCall,
        options: Vec<PermissionOption>,
        check: PathCheck,
    ) {
        let request = self.files_request();
        self.files.query(request, check.queries());
        let key = (session.clone(), permission);
        self.permissions.waiting.insert(key.clone(), options);
        self.permissions.resolving.insert(
            key,
            Resolving {
                request,
                call,
                check,
            },
        );
    }

    /// Decides the permission request that waits for `request` on its `answers`, and returns
    /// `None`; answers no request waits for are returned back untouched.
    ///
    /// # Errors
    ///
    /// As [`answer_permission`](Self::answer_permission), and [`AppError::UnknownSession`]
    /// when the session is gone.
    pub(super) fn paths_resolved(
        &mut self,
        request: FilesRequestId,
        answers: Vec<Result<FilesAnswer, FilesError>>,
    ) -> Result<Option<Vec<Result<FilesAnswer, FilesError>>>, AppError> {
        let Some(key) = self
            .permissions
            .resolving
            .iter()
            .find(|(_, resolving)| resolving.request == request)
            .map(|(key, _)| key.clone())
        else {
            return Ok(Some(answers));
        };
        let resolving = self.permissions.resolving.remove(&key);
        let options = self.permissions.waiting.remove(&key);
        let (Some(resolving), Some(options)) = (resolving, options) else {
            return Ok(None);
        };
        let (session, permission) = key;
        let decision = resolving.check.decide(answers);
        self.permission_decided(
            &session,
            permission,
            &resolving.call,
            options,
            true,
            decision,
        )?;
        Ok(None)
    }
}

#[cfg(test)]
mod tests;
