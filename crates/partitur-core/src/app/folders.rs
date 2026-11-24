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
//! cards on their next check, which follows every answer. Create and Approve always ask anew
//! when pressed, and their fresh reading of the repositories waits for those answers too, so
//! they decide on what the folder holds after the press, whichever answer comes first: a
//! folder that exists by then refuses them with `<path> already exists`.
//!
//! An answer to a question older than one already answered about the same folder is
//! dropped, so answers that arrive out of order never undo a newer one. Only the answers
//! about folders still shown, by the branch box or by a split card whose proposal is
//! undecided, are kept.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::App;
use crate::files::{FilesAnswer, FilesError, FilesQuery, FilesRequestId};
use crate::ids::{GitRequestId, ProposalId};

/// What shows the folders new worktrees would go to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum Owner {
    /// The branch box of the session being prepared.
    Draft,
    /// The split card of an undecided proposal.
    Proposal(ProposalId),
}

/// What the app knows about the folders new worktrees would go to.
#[derive(Debug, Default)]
pub(super) struct Folders {
    /// Whether each folder exists, by the answer to the newest question about it that was
    /// answered, with that question.
    known: HashMap<PathBuf, (FilesRequestId, bool)>,
    /// The questions on their way, each with the folder it asks about.
    asked: Vec<(FilesRequestId, PathBuf)>,
    /// The folders each owner shows now.
    pub(super) shown: HashMap<Owner, Vec<PathBuf>>,
}

impl Folders {
    /// Whether the latest answer about `path` says it exists; `false` before the first.
    pub(super) fn exists(&self, path: &Path) -> bool {
        self.known.get(path).is_some_and(|(_, exists)| *exists)
    }

    /// Records that `owner` shows the folders `paths` now, and forgets the answers about
    /// folders nothing shows any more.
    pub(super) fn show(&mut self, owner: Owner, paths: Vec<PathBuf>) {
        self.shown.insert(owner, paths);
        self.prune();
    }

    /// Records that `owner` shows no folders any more, as [`show`](Self::show) does.
    pub(super) fn hide(&mut self, owner: &Owner) {
        self.shown.remove(owner);
        self.prune();
    }

    /// Forgets the answers about folders nothing shows.
    fn prune(&mut self) {
        let shown = &self.shown;
        self.known
            .retain(|path, _| shown.values().any(|paths| paths.contains(path)));
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
        self.ask_folder_now(path);
    }

    /// Asks whether the folder `path` exists, even while an answer is on its way; the
    /// question's request.
    pub(super) fn ask_folder_now(&mut self, path: &Path) -> FilesRequestId {
        let request = self.files_request();
        self.files
            .query(request, vec![FilesQuery::Resolve(vec![path.to_path_buf()])]);
        self.folders.asked.push((request, path.to_path_buf()));
        request
    }

    /// Frees the repositories `hold` kept busy.
    pub(super) fn release(&mut self, hold: GitRequestId) {
        if let Some((_, freed)) = self.git_work.finish(hold) {
            self.forget_failed_reads(&freed);
        }
    }

    /// Takes the answer to the question `request` asked about a folder, kept while the folder
    /// is shown, and checks the branch box of the session being prepared again, without
    /// asking anything; a Create or an approval that waited for the answer goes on. `None`
    /// then.
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
        // An answer to an older question than the one known says less about the folder now.
        if self
            .folders
            .known
            .get(&path)
            .is_none_or(|(newest, _)| *newest < request)
        {
            self.folders.known.insert(path, (request, exists));
        }
        self.recheck_draft();
        self.draft_folder_checked(request);
        self.split_folder_checked(request);
        self.folders.prune();
        None
    }
}

#[cfg(test)]
mod tests;
