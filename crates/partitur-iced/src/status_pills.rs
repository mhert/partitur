//! The status pills that follow a breadcrumb, on the Session tab and on the Spec tab: the
//! `orchestrator` kind, the session's status, the mark of a session below it that waits for the
//! user and how many of its children are done. The projects tab's rows use the same mark.

use iced::Element;
use iced::widget::text;
use partitur_core::{SessionNode, SessionViewModel, Status, background_shown};

use crate::app::Message;
use crate::{badge, style, theme};

/// The pill that marks a session with a session below it that waits for the user.
pub(crate) const CHILD_ASKS_YOU: &str = "child asks you";

/// What follows the breadcrumb: the pills (`orchestrator` for an orchestrator, the
/// session's status, outlined while it is idle, and a mark when a session below it waits for the
/// user) and, for a session with children, how many of them are done.
pub(crate) fn trailing<'a>(
    model: &SessionViewModel<'_>,
) -> impl Iterator<Item = Element<'a, Message>> {
    let progress = (!model.children.is_empty()).then(|| {
        let done = model
            .children
            .iter()
            .filter(|child| {
                child.status == Status::Done && !background_shown(child.status, child.background)
            })
            .count();
        format!("{done} of {} children done", model.children.len())
    });
    let small = |words: String| text(words).size(theme::SMALL);
    [
        model.orchestrator.then(badge::orchestrator),
        Some(badge::session(
            shown_status(model),
            model.background,
            model.delegated,
        )),
        model
            .attention
            .then(|| badge::pill(CHILD_ASKS_YOU, |tokens| tokens.asks_you)),
        progress
            .is_some()
            .then(|| small("·".to_owned()).style(style::dim).into()),
        progress.map(|progress| small(progress).style(style::muted).into()),
    ]
    .into_iter()
    .flatten()
}

/// Where the session stands, as its pill says it: its own status, except that a session
/// that is done while a child of it is in a turn shows as running, like its row in the
/// tree, since the work is not over.
pub(crate) fn shown_status(model: &SessionViewModel<'_>) -> Status {
    if model.status == Status::Done && model.children.iter().any(in_turn) {
        Status::Running
    } else {
        model.status
    }
}

/// Whether `child` is in a turn: it runs, or it waits for the user's answer to a
/// permission request (a child proposes nothing, so it waits for nothing else).
pub(crate) fn in_turn(child: &SessionNode) -> bool {
    matches!(child.status, Status::Running | Status::WaitingForYou)
}
