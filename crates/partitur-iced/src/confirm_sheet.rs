//! The confirmation sheet: the question shown over the window before a session is archived
//! or deleted.
//!
//! The window stays visible under a dimming backdrop. The sheet names the session, tells
//! whether it is an orchestrator, how many child sessions go with it and where it stands,
//! then what the action means.
//!
//! Archiving keeps the transcript and the Env data: the session moves under `archived` in
//! its project and can be restored, and archived sessions leave the status counts and the
//! jump panel. `Archive` (or `Stop and archive`, when a turn is still going) archives it.
//!
//! Deleting removes the transcript and the Env data for good and cannot be undone; for a
//! session that is not archived yet the sheet points to archiving as the way to keep it.
//! `Delete` (or `Stop and delete`) deletes it, in the colour of failures.
//!
//! The confirm button acts and closes the sheet; `Cancel`, Escape or a click on the
//! backdrop closes the sheet and changes nothing. Enter confirms. While the sheet is open no
//! other key reaches the window. When the action fails the sheet stays open and says why.

use iced::widget::{Row, button, column, container, mouse_area, opaque, rich_text, row, span};
use iced::widget::{space, stack, text};
use iced::{Alignment, Border, Element, Length, Task, Theme};
use partitur_core::{SessionId, SessionSummary, Status};

use crate::app::{BareKey, Message, State, report};
use crate::theme::Tokens;
use crate::{badge, projects_view, style, theme};

/// The width of the sheet, in logical pixels.
const WIDTH: f32 = 560.0;

/// The room the sheet keeps around its content: above and below, then left and right.
const PADDING: [f32; 2] = [20.0, 22.0];

/// The space between the parts of the sheet.
const GAP: f32 = 16.0;

/// The corner radius of the sheet.
const RADIUS: f32 = 10.0;

/// The size of the sheet's title.
const TITLE_SIZE: f32 = 15.0;

/// The room the box that tells what the action means keeps around its text.
const BOX_PADDING: [f32; 2] = [10.0, 12.0];

/// The corner radius of the box that tells what the action means.
const BOX_RADIUS: f32 = 6.0;

/// The line height of the box that tells what the action means, in logical pixels.
const BOX_LINE_HEIGHT: f32 = 18.0;

/// How much of the window's colour the backdrop lays over the page.
const BACKDROP_STRENGTH: f32 = 0.72;

/// What the sheet asks to confirm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Archiving: the session leaves the tree and can be restored.
    Archive,
    /// Deleting: the session is gone for good.
    Delete,
}

impl Action {
    /// The action as a button names it: `Archive` or `Delete`.
    fn verb(self) -> &'static str {
        match self {
            Self::Archive => "Archive",
            Self::Delete => "Delete",
        }
    }

    /// The action in lower case, which also starts the ids of the sheet's buttons.
    fn word(self) -> &'static str {
        match self {
            Self::Archive => "archive",
            Self::Delete => "delete",
        }
    }
}

/// The sheet while it is open: the action, and the session it acts on as the core
/// summarized it when it opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmSheet {
    pub(crate) action: Action,
    pub(crate) summary: SessionSummary,
    /// Whether the session is archived already, so deleting it has no archive to point to.
    pub(crate) archived: bool,
}

/// An interaction with the sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmMessage {
    /// The user confirmed: the session is archived or deleted.
    Confirm,
    /// The user cancelled: the sheet closes.
    Cancel,
}

/// Opens the sheet that asks to `action` the `session`. When the core refuses (it is a
/// child, it is archived already and would be archived again, or it is gone), the error bar
/// says why and the sheet stays closed.
pub(crate) fn open(state: &mut State, action: Action, session: &SessionId) -> Task<Message> {
    let summary = match action {
        Action::Archive => state.app.archive_summary(session),
        Action::Delete => state.app.delete_summary(session),
    };
    if let Some(summary) = report(&mut state.error, summary) {
        let archived = state
            .app
            .tree()
            .projects
            .iter()
            .any(|project| project.archived.iter().any(|top| &top.id == session));
        state.error = None;
        state.confirm = Some(ConfirmSheet {
            action,
            summary,
            archived,
        });
    }
    Task::none()
}

/// Applies `message` to the open sheet. Confirming archives or deletes the session and
/// closes the sheet, or keeps it open with the error when that fails; cancelling closes it.
/// Nothing happens while the sheet is closed.
pub fn update(state: &mut State, message: ConfirmMessage) -> Task<Message> {
    let Some(sheet) = state.confirm.as_ref() else {
        return Task::none();
    };
    match message {
        ConfirmMessage::Confirm => {
            let session = sheet.summary.id.clone();
            let done = match sheet.action {
                Action::Archive => projects_view::archive(state, session),
                Action::Delete => projects_view::delete(state, session),
            };
            if done {
                state.confirm = None;
            }
        }
        ConfirmMessage::Cancel => {
            state.confirm = None;
            state.error = None;
        }
    }
    Task::none()
}

/// What the bare `key` does while the sheet is open: Enter confirms, Escape cancels, and
/// every other key does nothing, since the window under the sheet takes no keys.
pub(crate) fn press(state: &mut State, key: BareKey) -> Task<Message> {
    match key {
        BareKey::Enter => update(state, ConfirmMessage::Confirm),
        BareKey::Escape => update(state, ConfirmMessage::Cancel),
        BareKey::Tab
        | BareKey::ArrowDown
        | BareKey::ArrowUp
        | BareKey::N
        | BareKey::A
        | BareKey::D => Task::none(),
    }
}

/// `page` with `sheet` over it, the error of a failed action in the sheet when there is
/// one. The backdrop takes every click meant for the page; a click on it cancels.
pub fn view<'a>(
    page: Element<'a, Message>,
    sheet: &ConfirmSheet,
    error: Option<&str>,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let summary = &sheet.summary;
    let meaning = match sheet.action {
        Action::Archive => what_stays(&summary.project, tokens),
        Action::Delete => what_goes(sheet.archived, tokens),
    };
    let mut content = column![
        column![
            text(format!("{} “{}”", sheet.action.verb(), summary.title))
                .size(TITLE_SIZE)
                .font(theme::UI_SEMIBOLD),
            text(subline(summary))
                .size(theme::SMALL)
                .style(style::muted),
        ]
        .spacing(4),
        meaning,
    ]
    .spacing(GAP);
    if let Some(error) = error {
        content = content.push(
            text(error.to_owned())
                .size(theme::SMALL)
                .style(style::ink(|tokens| tokens.failed.fg)),
        );
    }
    content = content.push(actions(sheet.action, summary));
    let card = container(content)
        .width(WIDTH)
        .padding(PADDING)
        .style(sheet_style);
    // The sheet is opaque to clicks, so only a click beside it reaches the backdrop.
    let backdrop = mouse_area(container(opaque(card)).center(Length::Fill).style(
        |theme: &Theme| {
            iced::widget::container::Style {
                background: Some(
                    theme::tokens(theme)
                        .bg
                        .scale_alpha(BACKDROP_STRENGTH)
                        .into(),
                ),
                ..iced::widget::container::Style::default()
            }
        },
    ))
    .on_press(Message::Confirm(ConfirmMessage::Cancel));
    stack![page, opaque(backdrop)].into()
}

/// What the session is, how many sessions go with it and where it stands:
/// `orchestrator session · 2 child sessions · done`, or `session · done`.
fn subline(summary: &SessionSummary) -> String {
    let status = status_word(summary.status);
    if !summary.orchestrator {
        return format!("session · {status}");
    }
    let children = match summary.children {
        1 => "1 child session".to_owned(),
        count => format!("{count} child sessions"),
    };
    format!("orchestrator session · {children} · {status}")
}

/// The word the status pill shows for `status`, and `idle` for a session without one.
fn status_word(status: Status) -> &'static str {
    match status {
        Status::Idle => "idle",
        Status::Running => "running",
        Status::WaitingForYou => "asks you",
        Status::Done => "done",
        Status::Failed => "failed",
    }
}

/// The box that tells what archiving keeps, where the session goes in `project` and that
/// archived sessions leave the counts and the jump panel.
fn what_stays<'a>(project: &str, tokens: &Tokens) -> Element<'a, Message> {
    let kept: text::Rich<'a, (), Message> = rich_text![
        span("Transcript and Env are kept. The session moves under "),
        span("archived").color(tokens.text),
        span(format!(" in {project} and can be restored any time.")),
    ]
    .size(theme::SMALL)
    .line_height(box_line_height())
    .color(tokens.muted);
    let excluded = Row::new()
        .push(small("Excluded from status counts and from"))
        .push(badge::key_cap("Ctrl K"))
        .push(small("unless “include archived” is on."))
        .spacing(4)
        .align_y(Alignment::Center);
    meaning_box(column![kept, excluded].spacing(4).into())
}

/// The box that tells what deleting removes and that it cannot be undone, and, unless the
/// session is `archived` already, that archiving keeps it instead.
fn what_goes<'a>(archived: bool, tokens: &Tokens) -> Element<'a, Message> {
    let gone: text::Rich<'a, (), Message> = rich_text![
        span("Transcript and Env are deleted "),
        span("for good").color(tokens.text),
        span(". This cannot be undone."),
    ]
    .size(theme::SMALL)
    .line_height(box_line_height())
    .color(tokens.muted);
    let mut lines = column![gone].spacing(4);
    if !archived {
        lines = lines.push(
            Row::new()
                .push(small("To keep it out of sight instead, archive it"))
                .push(badge::key_cap("A"))
                .spacing(4)
                .align_y(Alignment::Center),
        );
    }
    meaning_box(lines.into())
}

/// The line height of the text in the box that tells what the action means.
fn box_line_height() -> text::LineHeight {
    text::LineHeight::Absolute(BOX_LINE_HEIGHT.into())
}

/// The box around `content`, which tells what the action means: the window colour with
/// rounded corners.
fn meaning_box(content: Element<'_, Message>) -> Element<'_, Message> {
    container(content)
        .width(Length::Fill)
        .padding(BOX_PADDING)
        .style(|theme: &Theme| iced::widget::container::Style {
            background: Some(theme::tokens(theme).bg.into()),
            border: Border::default().rounded(BOX_RADIUS),
            ..iced::widget::container::Style::default()
        })
        .into()
}

/// The buttons, found by `<action>-confirm` and `<action>-cancel` (`archive-confirm`,
/// `delete-cancel`), and on the right, for an orchestrator with children, the hint that they
/// go with it.
fn actions<'a>(action: Action, summary: &SessionSummary) -> Element<'a, Message> {
    let label = if summary.stops {
        format!("Stop and {}", action.word())
    } else {
        action.verb().to_owned()
    };
    let confirm_style = match action {
        Action::Archive => style::primary_button,
        Action::Delete => style::danger_primary_button,
    };
    let confirm = button(
        row![
            text(label).font(theme::UI_SEMIBOLD),
            badge::key_cap_on_primary("Enter")
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([6, 14])
    .style(confirm_style)
    .on_press(Message::Confirm(ConfirmMessage::Confirm));
    let cancel = button(
        row![text("Cancel"), badge::key_cap("Esc")]
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .padding([6, 14])
    .style(style::outlined_button)
    .on_press(Message::Confirm(ConfirmMessage::Cancel));
    let mut actions = row![
        container(confirm).id(format!("{}-confirm", action.word())),
        container(cancel).id(format!("{}-cancel", action.word())),
        space::horizontal(),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    if summary.orchestrator && summary.children > 0 {
        actions = actions.push(small(match action {
            Action::Archive => "children archive with it",
            Action::Delete => "children are deleted with it",
        }));
    }
    actions.into()
}

/// Small muted text.
fn small(label: &str) -> Element<'_, Message> {
    text(label).size(theme::SMALL).style(style::muted).into()
}

/// The sheet's surface: the panel colour inside a one-pixel outline in the input border
/// colour, with rounder corners than a card.
fn sheet_style(theme: &Theme) -> iced::widget::container::Style {
    let tokens = theme::tokens(theme);
    iced::widget::container::Style {
        background: Some(tokens.panel.into()),
        border: Border::default()
            .rounded(RADIUS)
            .width(1)
            .color(tokens.border_input),
        ..iced::widget::container::Style::default()
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use futures::channel::mpsc;
    use iced::Point;
    use iced::widget::Id;
    use iced_test::simulator;
    use partitur_core::testing::{MemoryStore, RecordingBackend, single_claude};
    use partitur_core::{App, BackendEvent, Scope, StopReason, Update};
    use tempfile::TempDir;

    use super::*;
    use crate::app::{Tab, boot, update as app_update, view as app_view};
    use crate::projects_view::ProjectsMessage;
    use crate::test_support::{attach, deliver, orchestrated};

    /// A state whose only project holds one session that finished its first turn, titled by
    /// its prompt `title`, with the backend, the store and the folder, which must outlive the
    /// state.
    fn finished(title: &str) -> (State, SessionId, MemoryStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let backend = RecordingBackend::new();
        let store = MemoryStore::new();
        let (backends, default) = single_claude();
        let app = App::load(
            Box::new(backend.clone()),
            Box::new(store.clone()),
            backends,
            default,
        )
        .unwrap();
        let mut state = boot(app, mpsc::unbounded().1).0;
        let project = state.app.open_project(dir.path()).unwrap();
        let session = state.app.new_session(project, Scope::Project).unwrap();
        attach(&mut state, &backend, &session, "b");
        state.app.submit_prompt(title, Vec::new()).unwrap();
        deliver(
            &mut state,
            BackendEvent::Update {
                session: session.clone(),
                update: Update::TurnEnded(StopReason::EndTurn),
            },
        );
        state.tab = Tab::Projects;
        state.projects.cursor = Some(session.clone());
        (state, session, store, dir)
    }

    fn send(state: &mut State, message: Message) {
        let _task = app_update(state, message);
    }

    fn archived(state: &State) -> usize {
        state.app.tree().projects[0].archived.len()
    }

    /// How many top-level sessions, live or archived, the first project holds.
    fn top_level(state: &State) -> usize {
        let tree = state.app.tree();
        tree.projects[0].sessions.len() + tree.projects[0].archived.len()
    }

    /// The message that asks for `action` on `session`, as the Projects tab sends it.
    fn ask(action: Action, session: SessionId) -> Message {
        Message::Projects(match action {
            Action::Archive => ProjectsMessage::Archive(session),
            Action::Delete => ProjectsMessage::Delete(session),
        })
    }

    #[test]
    fn pressing_archive_opens_the_sheet_with_title_and_subline() {
        let (mut state, session, _store, _dir) = finished("Fix checkout 500");

        send(&mut state, ask(Action::Archive, session));

        assert_eq!(archived(&state), 0);
        let mut ui = simulator(app_view(&state));
        ui.find("Archive “Fix checkout 500”").unwrap();
        ui.find("session · done").unwrap();
        ui.find("Archive").unwrap();
        assert!(ui.find("children archive with it").is_err());
    }

    #[test]
    fn pressing_delete_opens_the_sheet_that_says_it_cannot_be_undone() {
        let (mut state, session, _store, _dir) = finished("Fix checkout 500");

        send(&mut state, ask(Action::Delete, session));

        assert_eq!(top_level(&state), 1);
        let mut ui = simulator(app_view(&state));
        ui.find("Delete “Fix checkout 500”").unwrap();
        ui.find("session · done").unwrap();
        ui.find("To keep it out of sight instead, archive it")
            .unwrap();
        ui.find("Delete").unwrap();
        ui.find(Id::new("delete-confirm")).unwrap();
        assert!(ui.find(Id::new("archive-confirm")).is_err());
        assert!(ui.find("children are deleted with it").is_err());
    }

    #[test]
    fn the_delete_sheet_of_an_archived_session_points_to_no_archive() {
        let (mut state, session, _store, _dir) = finished("Fix it");
        state.app.archive(session.clone()).unwrap();

        send(&mut state, ask(Action::Delete, session));

        assert!(state.confirm.as_ref().unwrap().archived);
        let mut ui = simulator(app_view(&state));
        ui.find("Delete “Fix it”").unwrap();
        assert!(
            ui.find("To keep it out of sight instead, archive it")
                .is_err()
        );
    }

    #[test]
    fn an_orchestrator_sheet_counts_its_children_and_stops_them() {
        let mut fixture = orchestrated();
        let orchestrator = fixture.orchestrator.clone();

        send(
            &mut fixture.state,
            ask(Action::Archive, orchestrator.clone()),
        );
        {
            let mut ui = simulator(app_view(&fixture.state));
            ui.find("orchestrator session · 1 child session · running")
                .unwrap();
            ui.find("Stop and archive").unwrap();
            ui.find("children archive with it").unwrap();
        }
        send(&mut fixture.state, Message::Confirm(ConfirmMessage::Cancel));

        send(&mut fixture.state, ask(Action::Delete, orchestrator));
        let mut ui = simulator(app_view(&fixture.state));
        ui.find("orchestrator session · 1 child session · running")
            .unwrap();
        ui.find("Stop and delete").unwrap();
        ui.find("children are deleted with it").unwrap();
    }

    #[test]
    fn a_child_opens_no_sheet() {
        for action in [Action::Archive, Action::Delete] {
            let mut fixture = orchestrated();
            let child = fixture.child.clone();

            send(&mut fixture.state, ask(action, child));

            assert_eq!(fixture.state.confirm, None);
            assert!(fixture.state.error.is_some());
        }
    }

    #[test]
    fn enter_archives_and_closes_the_sheet() {
        let (mut state, session, _store, _dir) = finished("Fix it");
        send(&mut state, ask(Action::Archive, session));

        send(&mut state, Message::Key(BareKey::Enter));

        assert_eq!(state.confirm, None);
        assert_eq!(archived(&state), 1);
        assert_eq!(state.tab, Tab::Projects);
    }

    #[test]
    fn enter_deletes_and_closes_the_sheet() {
        let (mut state, session, _store, _dir) = finished("Fix it");
        send(&mut state, ask(Action::Delete, session));

        send(&mut state, Message::Key(BareKey::Enter));

        assert_eq!(state.confirm, None);
        assert_eq!(state.error, None);
        assert_eq!(top_level(&state), 0);
        assert_eq!(state.tab, Tab::Projects);
    }

    #[test]
    fn enter_deletes_an_archived_session() {
        let (mut state, session, _store, _dir) = finished("Fix it");
        state.app.archive(session.clone()).unwrap();
        send(&mut state, ask(Action::Delete, session));

        send(&mut state, Message::Key(BareKey::Enter));

        assert_eq!(state.confirm, None);
        assert_eq!(top_level(&state), 0);
    }

    #[test]
    fn escape_cancels_and_changes_nothing() {
        for action in [Action::Archive, Action::Delete] {
            let (mut state, session, _store, _dir) = finished("Fix it");
            send(&mut state, ask(action, session));

            send(&mut state, Message::Key(BareKey::Escape));

            assert_eq!(state.confirm, None);
            assert_eq!(archived(&state), 0);
            assert_eq!(top_level(&state), 1);
        }
    }

    #[test]
    fn clicking_the_backdrop_cancels() {
        for action in [Action::Archive, Action::Delete] {
            let (mut state, session, _store, _dir) = finished("Fix it");
            send(&mut state, ask(action, session));
            let mut ui = simulator(app_view(&state));

            ui.point_at(Point::new(20.0, 600.0));
            ui.simulate(iced_test::simulator::click());

            let messages: Vec<Message> = ui.into_messages().collect();
            assert_eq!(messages, [Message::Confirm(ConfirmMessage::Cancel)]);
        }
    }

    #[test]
    fn the_buttons_confirm_and_cancel() {
        for action in [Action::Archive, Action::Delete] {
            let (mut state, session, _store, _dir) = finished("Fix it");
            send(&mut state, ask(action, session));

            for (button, expected) in [
                ("confirm", ConfirmMessage::Confirm),
                ("cancel", ConfirmMessage::Cancel),
            ] {
                let mut ui = simulator(app_view(&state));
                ui.click(Id::from(format!("{}-{button}", action.word())))
                    .unwrap();
                let messages: Vec<Message> = ui.into_messages().collect();
                assert_eq!(messages, [Message::Confirm(expected)]);
            }
        }
    }

    #[test]
    fn keys_other_than_enter_and_escape_do_nothing_while_the_sheet_is_open() {
        for action in [Action::Archive, Action::Delete] {
            let (mut state, session, _store, _dir) = finished("Fix it");
            send(&mut state, ask(action, session.clone()));

            for key in [
                BareKey::Tab,
                BareKey::ArrowDown,
                BareKey::N,
                BareKey::A,
                BareKey::D,
            ] {
                send(&mut state, Message::Key(key));
            }
            send(&mut state, Message::Jump(crate::jump::JumpMessage::Open));
            send(&mut state, Message::SelectTab(Tab::Env));

            assert_eq!(state.tab, Tab::Projects);
            assert_eq!(state.projects.cursor, Some(session));
            assert_eq!(state.jump, None);
            assert_eq!(state.confirm.as_ref().unwrap().action, action);
            assert_eq!(state.app.tree().projects[0].sessions.len(), 1);
            assert_eq!(archived(&state), 0);
        }
    }

    #[test]
    fn a_failed_archive_keeps_the_sheet_open_with_the_error() {
        let mut fixture = orchestrated();
        let orchestrator = fixture.orchestrator.clone();
        send(&mut fixture.state, ask(Action::Archive, orchestrator));

        fixture.backend.fail_next_call();
        send(
            &mut fixture.state,
            Message::Confirm(ConfirmMessage::Confirm),
        );

        assert!(fixture.state.confirm.is_some());
        let error = fixture.state.error.clone().unwrap();
        {
            let mut ui = simulator(app_view(&fixture.state));
            assert!(crate::test_support::shown(&mut ui, &error) >= 1);
        }
        assert_eq!(fixture.state.app.tree().projects[0].archived.len(), 0);

        send(&mut fixture.state, Message::Confirm(ConfirmMessage::Cancel));
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn a_failed_delete_keeps_the_sheet_open_with_the_error() {
        let (mut state, session, store, _dir) = finished("Fix it");
        send(&mut state, ask(Action::Delete, session));

        store.fail_next_session_save();
        send(&mut state, Message::Key(BareKey::Enter));

        assert!(state.confirm.is_some());
        let error = state.error.clone().unwrap();
        {
            let mut ui = simulator(app_view(&state));
            assert!(crate::test_support::shown(&mut ui, &error) >= 1);
        }
        assert_eq!(top_level(&state), 1);

        send(&mut state, Message::Key(BareKey::Escape));
        assert_eq!(state.confirm, None);
        assert_eq!(state.error, None);
    }

    #[test]
    fn a_delete_that_fails_after_the_session_is_gone_closes_the_sheet_with_the_error() {
        let (mut state, session, store, _dir) = finished("Fix it");
        send(&mut state, ask(Action::Delete, session));

        store.fail_next_delete();
        send(&mut state, Message::Key(BareKey::Enter));

        assert_eq!(state.confirm, None);
        assert!(state.error.is_some());
        assert_eq!(top_level(&state), 0);
    }

    #[test]
    fn a_closed_sheet_ignores_its_messages() {
        let (mut state, _session, _store, _dir) = finished("Fix it");

        send(&mut state, Message::Confirm(ConfirmMessage::Confirm));

        assert_eq!(archived(&state), 0);
        assert_eq!(top_level(&state), 1);
    }
}
