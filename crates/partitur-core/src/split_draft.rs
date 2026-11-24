//! The undecided split as the user is editing it, and the rules of its approval card: which
//! children can be approved, what blocks the approval, the choices the card hands to
//! [`App::approve_proposal`] and the note that goes with a rejection.
//!
//! This is not `SplitState` in `app/split.rs`, which holds the git
//! work of an approval that is under way: a [`SplitDraft`] is what the card holds before
//! anything is decided. Its children are [`ChildChoice`]s, the values the card leaves the
//! approval with, kept as typed: the prompts and titles are trimmed only when the draft is
//! turned into [choices](SplitDraft::choices). The draft holds plain data. What the card shows
//! of it, such as which forms are open and the text editors of the prompts, belongs to the UI.
//!
//! The draft keeps the last [check](SplitDraft::check) of its children, as the core made it in
//! their own repositories; a child is ready once git described its repository and the check
//! found no problem with it.

use crate::app::{App, AppError, ChildChoice};
use crate::backend::ProposedChild;
use crate::ids::{ProposalId, SessionId};
use crate::session::BranchChoice;
use crate::view_model::ChildCheck;

/// An undecided proposal as the user is editing it on its card.
#[derive(Debug)]
pub struct SplitDraft {
    /// The children to start, in order, as typed.
    pub children: Vec<ChildChoice>,
    /// The note that goes to the orchestrator with a rejection, as typed.
    pub note: String,
    /// How each child would start, in order, as the core checked them last; why the core
    /// could not check them otherwise.
    pub checks: Result<Vec<ChildCheck>, String>,
}

/// The first child a check found a problem with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blocker<'a> {
    /// The child's position.
    pub index: usize,
    /// What is wrong with it.
    pub problem: &'a str,
    /// How many children have a problem in all, the first one included.
    pub count: usize,
}

impl SplitDraft {
    /// A draft of the proposed `children` as the orchestrator proposed them, each granting
    /// permission requests without asking when `auto_allow` and starting from the branch
    /// settings `branch` gives it, without a note and not checked yet.
    pub fn from_entry(
        children: &[ProposedChild],
        auto_allow: bool,
        branch: impl Fn(&ProposedChild) -> BranchChoice,
    ) -> Self {
        let children = children
            .iter()
            .map(|child| ChildChoice {
                repository: child.repository.clone(),
                prompt: child.prompt.clone(),
                title: child.title.clone(),
                auto_allow,
                branch: branch(child),
            })
            .collect();
        Self {
            children,
            note: String::new(),
            checks: Ok(Vec::new()),
        }
    }

    /// The children to approve, with their prompts and titles trimmed.
    #[must_use]
    pub fn choices(&self) -> Vec<ChildChoice> {
        self.children
            .iter()
            .map(|child| ChildChoice {
                prompt: child.prompt.trim().to_owned(),
                title: child.title.trim().to_owned(),
                ..child.clone()
            })
            .collect()
    }

    /// Lets the core check the children as they are now, as the children of the proposal
    /// `id` of the orchestrator `session`; returns whether the proposal is still undecided
    /// and its orchestrator still there, so the draft is still wanted. A check the user
    /// caused, `by_user`, reads the repositories git failed to read again; any other keeps
    /// what git said.
    pub fn check(
        &mut self,
        app: &mut App,
        session: &SessionId,
        id: &ProposalId,
        by_user: bool,
    ) -> bool {
        let choices = self.choices();
        let checked = if by_user {
            app.check_split(session, id, &choices)
        } else {
            app.recheck_split(session, id, &choices)
        };
        let wanted = !matches!(
            checked,
            Err(AppError::UnknownProposal(_) | AppError::UnknownSession(_))
        );
        self.checks = checked.map_err(|error| error.to_string());
        wanted
    }

    /// What the last check said about the child at `index`.
    #[must_use]
    pub fn check_of(&self, index: usize) -> Option<&ChildCheck> {
        self.checks
            .as_ref()
            .ok()
            .and_then(|checks| checks.get(index))
    }

    /// Whether the split can be approved: it has children, and the last check found every
    /// one of them read and without a problem.
    #[must_use]
    pub fn approvable(&self) -> bool {
        let ready = |checks: &Vec<ChildCheck>| {
            checks.len() == self.children.len()
                && checks
                    .iter()
                    .all(|check| !check.loading && check.problem.is_none())
        };
        !self.children.is_empty() && self.checks.as_ref().is_ok_and(ready)
    }

    /// The first child the last check found a problem with and how many children have one
    /// in all; `None` when no child has one.
    #[must_use]
    pub fn blocker(&self) -> Option<Blocker<'_>> {
        let checks = self.checks.as_ref().ok()?;
        let mut problems = checks
            .iter()
            .enumerate()
            .filter_map(|(index, check)| Some((index, check.problem.as_deref()?)));
        let (index, problem) = problems.next()?;
        Some(Blocker {
            index,
            problem,
            count: problems.count().saturating_add(1),
        })
    }

    /// The note to send with a rejection; none when it is blank.
    #[must_use]
    pub fn rejection_note(&self) -> Option<String> {
        let note = self.note.trim();
        (!note.is_empty()).then(|| note.to_owned())
    }
}

#[cfg(test)]
mod tests;
