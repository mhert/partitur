//! Whether the folders new worktrees would go to exist, asked of the files port in the
//! background.
//!
//! The branch box of the session being prepared and the children of a proposed split refuse
//! a worktree whose folder exists already. Whether it does is asked through the files port
//! each time the box or a card is checked because something changed, or Create or Approve
//! is pressed, unless an answer for that folder is on its way already; the latest answer
//! for each folder is kept. A check that only takes in what arrived asks only about folders
//! never asked about, so an answer never starts the next question.
//!
//! A folder counts as missing until its first answer arrives: nothing is shown meanwhile,
//! and when the answer says it exists, the branch box is checked again at once and the split
//! cards on their next check, which follows every answer. Create and Approve check the
//! folders once more with the fresh reading of their repositories that they wait for anyway,
//! which the answer to the question they asked when pressed has reached by then, as the files
//! port answers far faster than git. Should it not have, git itself refuses to add a worktree
//! over a folder that holds anything, so nothing is made over it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::App;
use crate::files::{FilesAnswer, FilesError, FilesQuery, FilesRequestId};

/// What the app knows about the folders new worktrees would go to.
#[derive(Debug, Default)]
pub(super) struct Folders {
    /// Whether each folder exists, by the latest answer about it.
    known: HashMap<PathBuf, bool>,
    /// The questions on their way, each with the folder it asks about.
    asked: Vec<(FilesRequestId, PathBuf)>,
}

impl Folders {
    /// Whether the latest answer about `path` says it exists; `false` before the first.
    pub(super) fn exists(&self, path: &Path) -> bool {
        self.known.get(path).copied().unwrap_or(false)
    }
}

impl App {
    /// Asks whether the folder `path` exists, unless that is on its way already; with
    /// `again` unset, only when it was never answered either.
    pub(super) fn ask_folder(&mut self, path: &Path, again: bool) {
        let folders = &self.folders;
        if folders.asked.iter().any(|(_, asked)| asked == path)
            || (!again && folders.known.contains_key(path))
        {
            return;
        }
        let request = self.files_request();
        self.files
            .query(request, vec![FilesQuery::Resolve(vec![path.to_path_buf()])]);
        self.folders.asked.push((request, path.to_path_buf()));
    }

    /// Takes the answer to the question `request` asked about a folder and checks the branch
    /// box of the session being prepared again, without asking anything; `None` then.
    /// Answers no question waits for are returned back untouched. A folder the port could
    /// not tell about counts as missing.
    pub(super) fn folder_answered(
        &mut self,
        request: FilesRequestId,
        answers: Vec<Result<FilesAnswer, FilesError>>,
    ) -> Option<Vec<Result<FilesAnswer, FilesError>>> {
        let Some(at) = self
            .folders
            .asked
            .iter()
            .position(|(asked, _)| *asked == request)
        else {
            return Some(answers);
        };
        let (_, path) = self.folders.asked.remove(at);
        let exists = matches!(
            answers.first(),
            Some(Ok(FilesAnswer::Resolved(resolved)))
                if resolved.iter().any(|fact| fact.given == path && fact.canonical.is_some())
        );
        self.folders.known.insert(path, exists);
        self.recheck_draft();
        None
    }
}

#[cfg(test)]
mod tests;
