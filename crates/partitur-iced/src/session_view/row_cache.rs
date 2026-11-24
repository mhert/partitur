//! The rows of the shown session's transcript, kept between messages.
//!
//! Building the rows walks every entry of the transcript, and every message asks for them
//! several times over: for the window of rows the Session tab keeps, for the widgets that
//! follow it and for the view. So they are built once and kept for what they depend on, and
//! built again only when one of those changes: the session, the revision of its transcript,
//! how many of its children's permission requests wait for the user, and whether any of its
//! children is in a turn. A transcript is loaded once while its session is in the tree, and
//! every update applied to it changes its revision; a session that leaves the tree has its
//! rows dropped, so a transcript loaded again starts from fresh rows. Nothing else goes into
//! the rows: whether a row is expanded, the theme, a widget's frame, the width and the
//! measured heights are taken from the Session tab and the session when a row is laid out,
//! so none of them leaves the rows stale. The view checks what the rows kept were built for,
//! and builds its own when they do not fit.

use std::borrow::Cow;
#[cfg(test)]
use std::cell::Cell;

use partitur_core::{SessionId, SessionViewModel};

use super::entries::{self, TranscriptRow};
use super::in_turn;

/// The rows of one session's transcript, with what they were built for.
#[derive(Debug, Default)]
pub(crate) struct RowCache {
    /// The rows kept, and what they were built for; nothing before the first build and after
    /// their session was forgotten.
    kept: Option<(Basis, Vec<TranscriptRow>)>,
    /// How many times rows were built, kept or not.
    #[cfg(test)]
    builds: Cell<usize>,
}

/// Everything about a session the rows of its transcript depend on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Basis {
    /// The session.
    session: SessionId,
    /// The revision of its transcript, which changes with every update applied to it.
    revision: u64,
    /// How many permission requests of its children wait for the user, a row each.
    child_permissions: usize,
    /// Whether one of its children is in a turn, which adds the row of what is pending.
    children_in_turn: bool,
}

impl Basis {
    /// What the rows of the session `model` depend on now.
    fn of(model: &SessionViewModel<'_>) -> Self {
        Self {
            session: model.id.clone(),
            revision: model.revision,
            child_permissions: model.child_permissions.len(),
            children_in_turn: model.children.iter().any(in_turn),
        }
    }
}

impl RowCache {
    /// Brings the rows kept up to date with the session `model`, building them only when what
    /// they depend on changed, and returns them.
    pub(super) fn refresh(&mut self, model: &SessionViewModel<'_>) -> &[TranscriptRow] {
        let basis = Basis::of(model);
        if self.kept.as_ref().is_none_or(|(kept, _)| *kept != basis) {
            let rows = self.build(model);
            self.kept = Some((basis, rows));
        }
        self.kept.as_ref().map_or(&[], |(_, rows)| rows.as_slice())
    }

    /// The rows of the session `model`: those kept when they were built for it as it is now,
    /// else rows built for it at once, which are not kept.
    pub(super) fn rows(&self, model: &SessionViewModel<'_>) -> Cow<'_, [TranscriptRow]> {
        match &self.kept {
            Some((kept, rows)) if *kept == Basis::of(model) => Cow::Borrowed(rows),
            _ => Cow::Owned(self.build(model)),
        }
    }

    /// Drops the rows kept when they belong to one of `sessions`, which left the tree.
    pub(super) fn forget(&mut self, sessions: &[SessionId]) {
        if self
            .kept
            .as_ref()
            .is_some_and(|(kept, _)| sessions.contains(&kept.session))
        {
            self.kept = None;
        }
    }

    /// The rows kept, whatever they were built for.
    #[cfg(test)]
    pub(super) fn kept(&self) -> Option<&[TranscriptRow]> {
        self.kept.as_ref().map(|(_, rows)| rows.as_slice())
    }

    /// How many times rows were built, kept or not.
    #[cfg(test)]
    pub(crate) fn builds(&self) -> usize {
        self.builds.get()
    }

    /// The rows of the session `model`, built anew.
    #[cfg_attr(
        not(test),
        expect(clippy::unused_self, reason = "only the tests count the builds")
    )]
    fn build(&self, model: &SessionViewModel<'_>) -> Vec<TranscriptRow> {
        #[cfg(test)]
        self.builds.set(self.builds.get().saturating_add(1));
        entries::rows(model)
    }
}
