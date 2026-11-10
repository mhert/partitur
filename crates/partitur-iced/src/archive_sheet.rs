//! The archive sheet: the confirmation that archives a session, shown over the window.
//!
//! The window stays visible under a dimming backdrop. The sheet names the session, tells
//! whether it is an orchestrator, how many child sessions archive with it and where it
//! stands, then what stays: the transcript and the Env data are kept, the session moves
//! under `archived` in its project and can be restored, and archived sessions leave the
//! status counts and the jump panel. `Archive` (or `Stop and archive`, when a turn is still
//! going) archives it and closes the sheet; `Cancel`, Escape or a click on the backdrop
//! closes the sheet and changes nothing. Enter archives. While the sheet is open no other
//! key reaches the window. When archiving fails the sheet stays open and says why.

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

/// The room the box that tells what stays keeps around its text.
const BOX_PADDING: [f32; 2] = [10.0, 12.0];

/// The corner radius of the box that tells what stays.
const BOX_RADIUS: f32 = 6.0;

/// The line height of the box that tells what stays, in logical pixels.
const BOX_LINE_HEIGHT: f32 = 18.0;

/// How much of the window's colour the backdrop lays over the page.
const BACKDROP_STRENGTH: f32 = 0.72;

/// The sheet while it is open: the session it archives, as the core summarized it when it
/// opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveSheet {
    pub(crate) summary: SessionSummary,
}

/// An interaction with the sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveMessage {
    /// The user confirmed: the session is archived.
    Confirm,
    /// The user cancelled: the sheet closes.
    Cancel,
}

/// Opens the sheet for `session`. When the core refuses to archive it (it is a child, it
/// is archived already or it is gone), the error bar says why and the sheet stays closed.
pub(crate) fn open(state: &mut State, session: &SessionId) -> Task<Message> {
    if let Some(summary) = report(&mut state.error, state.app.archive_summary(session)) {
        state.error = None;
        state.archive = Some(ArchiveSheet { summary });
    }
    Task::none()
}

/// Applies `message` to the open sheet. Confirming archives the session and closes the
/// sheet, or keeps it open with the error when archiving fails; cancelling closes it.
/// Nothing happens while the sheet is closed.
pub fn update(state: &mut State, message: ArchiveMessage) -> Task<Message> {
    let Some(sheet) = state.archive.as_ref() else {
        return Task::none();
    };
    match message {
        ArchiveMessage::Confirm => {
            let session = sheet.summary.id.clone();
            if projects_view::archive(state, session) {
                state.archive = None;
            }
        }
        ArchiveMessage::Cancel => {
            state.archive = None;
            state.error = None;
        }
    }
    Task::none()
}

/// What the bare `key` does while the sheet is open: Enter archives, Escape cancels, and
/// every other key does nothing, since the window under the sheet takes no keys.
pub(crate) fn press(state: &mut State, key: BareKey) -> Task<Message> {
    match key {
        BareKey::Enter => update(state, ArchiveMessage::Confirm),
        BareKey::Escape => update(state, ArchiveMessage::Cancel),
        BareKey::Tab | BareKey::ArrowDown | BareKey::ArrowUp | BareKey::N | BareKey::A => {
            Task::none()
        }
    }
}

/// `page` with `sheet` over it, the error of a failed archive in the sheet when there is
/// one. The backdrop takes every click meant for the page; a click on it cancels.
pub fn view<'a>(
    page: Element<'a, Message>,
    sheet: &ArchiveSheet,
    error: Option<&str>,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let summary = &sheet.summary;
    let mut content = column![
        column![
            text(format!("Archive “{}”", summary.title))
                .size(TITLE_SIZE)
                .font(theme::UI_SEMIBOLD),
            text(subline(summary))
                .size(theme::SMALL)
                .style(style::muted),
        ]
        .spacing(4),
        what_stays(&summary.project, tokens),
    ]
    .spacing(GAP);
    if let Some(error) = error {
        content = content.push(
            text(error.to_owned())
                .size(theme::SMALL)
                .style(style::ink(|tokens| tokens.failed.fg)),
        );
    }
    content = content.push(actions(summary));
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
    .on_press(Message::Archive(ArchiveMessage::Cancel));
    stack![page, opaque(backdrop)].into()
}

/// What the session is, how many sessions archive with it and where it stands:
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
    let line_height = text::LineHeight::Absolute(BOX_LINE_HEIGHT.into());
    let kept: text::Rich<'a, (), Message> = rich_text![
        span("Transcript and Env are kept. The session moves under "),
        span("archived").color(tokens.text),
        span(format!(" in {project} and can be restored any time.")),
    ]
    .size(theme::SMALL)
    .line_height(line_height)
    .color(tokens.muted);
    let excluded = Row::new()
        .push(small("Excluded from status counts and from"))
        .push(badge::key_cap("Ctrl K"))
        .push(small("unless “include archived” is on."))
        .spacing(4)
        .align_y(Alignment::Center);
    container(column![kept, excluded].spacing(4))
        .width(Length::Fill)
        .padding(BOX_PADDING)
        .style(|theme: &Theme| iced::widget::container::Style {
            background: Some(theme::tokens(theme).bg.into()),
            border: Border::default().rounded(BOX_RADIUS),
            ..iced::widget::container::Style::default()
        })
        .into()
}

/// The buttons, found by `archive-confirm` and `archive-cancel`, and on the right, for an
/// orchestrator with children, the hint that they archive with it.
fn actions<'a>(summary: &SessionSummary) -> Element<'a, Message> {
    let label = if summary.stops {
        "Stop and archive"
    } else {
        "Archive"
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
    .style(style::primary_button)
    .on_press(Message::Archive(ArchiveMessage::Confirm));
    let cancel = button(
        row![text("Cancel"), badge::key_cap("Esc")]
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .padding([6, 14])
    .style(style::outlined_button)
    .on_press(Message::Archive(ArchiveMessage::Cancel));
    let mut actions = row![
        container(confirm).id("archive-confirm"),
        container(cancel).id("archive-cancel"),
        space::horizontal(),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    if summary.orchestrator && summary.children > 0 {
        actions = actions.push(small("children archive with it"));
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
    /// its prompt `title`, with the backend and the folder, which must outlive the state.
    fn finished(title: &str) -> (State, SessionId, RecordingBackend, TempDir) {
        let dir = TempDir::new().unwrap();
        let backend = RecordingBackend::new();
        let (backends, default) = single_claude();
        let app = App::load(
            Box::new(backend.clone()),
            Box::new(MemoryStore::new()),
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
        (state, session, backend, dir)
    }

    fn send(state: &mut State, message: Message) {
        let _task = app_update(state, message);
    }

    fn archived(state: &State) -> usize {
        state.app.tree().projects[0].archived.len()
    }

    #[test]
    fn pressing_archive_opens_the_sheet_with_title_and_subline() {
        let (mut state, session, _backend, _dir) = finished("Fix checkout 500");

        send(
            &mut state,
            Message::Projects(ProjectsMessage::Archive(session.clone())),
        );

        assert_eq!(archived(&state), 0);
        let mut ui = simulator(app_view(&state));
        ui.find("Archive “Fix checkout 500”").unwrap();
        ui.find("session · done").unwrap();
        ui.find("Archive").unwrap();
        assert!(ui.find("children archive with it").is_err());
    }

    #[test]
    fn an_orchestrator_sheet_counts_its_children_and_stops_them() {
        let mut fixture = orchestrated();
        let orchestrator = fixture.orchestrator.clone();

        send(
            &mut fixture.state,
            Message::Projects(ProjectsMessage::Archive(orchestrator)),
        );

        let mut ui = simulator(app_view(&fixture.state));
        ui.find("orchestrator session · 1 child session · running")
            .unwrap();
        ui.find("Stop and archive").unwrap();
        ui.find("children archive with it").unwrap();
    }

    #[test]
    fn a_child_opens_no_sheet() {
        let mut fixture = orchestrated();
        let child = fixture.child.clone();

        send(
            &mut fixture.state,
            Message::Projects(ProjectsMessage::Archive(child)),
        );

        assert_eq!(fixture.state.archive, None);
        assert!(fixture.state.error.is_some());
    }

    #[test]
    fn enter_archives_and_closes_the_sheet() {
        let (mut state, session, _backend, _dir) = finished("Fix it");
        send(
            &mut state,
            Message::Projects(ProjectsMessage::Archive(session)),
        );

        send(&mut state, Message::Key(BareKey::Enter));

        assert_eq!(state.archive, None);
        assert_eq!(archived(&state), 1);
        assert_eq!(state.tab, Tab::Projects);
    }

    #[test]
    fn escape_cancels_and_leaves_the_session_live() {
        let (mut state, session, _backend, _dir) = finished("Fix it");
        send(
            &mut state,
            Message::Projects(ProjectsMessage::Archive(session)),
        );

        send(&mut state, Message::Key(BareKey::Escape));

        assert_eq!(state.archive, None);
        assert_eq!(archived(&state), 0);
    }

    #[test]
    fn clicking_the_backdrop_cancels() {
        let (mut state, session, _backend, _dir) = finished("Fix it");
        send(
            &mut state,
            Message::Projects(ProjectsMessage::Archive(session)),
        );
        let mut ui = simulator(app_view(&state));

        ui.point_at(Point::new(20.0, 600.0));
        ui.simulate(iced_test::simulator::click());

        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [Message::Archive(ArchiveMessage::Cancel)]);
    }

    #[test]
    fn the_buttons_archive_and_cancel() {
        let (mut state, session, _backend, _dir) = finished("Fix it");
        send(
            &mut state,
            Message::Projects(ProjectsMessage::Archive(session)),
        );

        for (id, expected) in [
            ("archive-confirm", ArchiveMessage::Confirm),
            ("archive-cancel", ArchiveMessage::Cancel),
        ] {
            let mut ui = simulator(app_view(&state));
            ui.click(Id::new(id)).unwrap();
            let messages: Vec<Message> = ui.into_messages().collect();
            assert_eq!(messages, [Message::Archive(expected)]);
        }
    }

    #[test]
    fn keys_other_than_enter_and_escape_do_nothing_while_the_sheet_is_open() {
        let (mut state, session, _backend, _dir) = finished("Fix it");
        send(
            &mut state,
            Message::Projects(ProjectsMessage::Archive(session.clone())),
        );

        for key in [BareKey::Tab, BareKey::ArrowDown, BareKey::N, BareKey::A] {
            send(&mut state, Message::Key(key));
        }
        send(&mut state, Message::Jump(crate::jump::JumpMessage::Open));
        send(&mut state, Message::SelectTab(Tab::Env));

        assert_eq!(state.tab, Tab::Projects);
        assert_eq!(state.projects.cursor, Some(session));
        assert_eq!(state.jump, None);
        assert!(state.archive.is_some());
        assert_eq!(state.app.tree().projects[0].sessions.len(), 1);
        assert_eq!(archived(&state), 0);
    }

    #[test]
    fn a_failed_archive_keeps_the_sheet_open_with_the_error() {
        let mut fixture = orchestrated();
        let orchestrator = fixture.orchestrator.clone();
        send(
            &mut fixture.state,
            Message::Projects(ProjectsMessage::Archive(orchestrator)),
        );

        fixture.backend.fail_next_call();
        send(
            &mut fixture.state,
            Message::Archive(ArchiveMessage::Confirm),
        );

        assert!(fixture.state.archive.is_some());
        let error = fixture.state.error.clone().unwrap();
        {
            let mut ui = simulator(app_view(&fixture.state));
            assert!(crate::test_support::shown(&mut ui, &error) >= 1);
        }
        assert_eq!(fixture.state.app.tree().projects[0].archived.len(), 0);

        send(&mut fixture.state, Message::Archive(ArchiveMessage::Cancel));
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn a_closed_sheet_ignores_its_messages() {
        let (mut state, _session, _backend, _dir) = finished("Fix it");

        send(&mut state, Message::Archive(ArchiveMessage::Confirm));

        assert_eq!(archived(&state), 0);
    }
}
