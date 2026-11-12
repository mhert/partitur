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
//! The delete sheet also offers to remove what the session's family made in git: one block
//! per session that made a branch, headed `<repository> · <title>`, with an unchecked box
//! `Remove worktree <path>` while git lists its worktree, warned with how many uncommitted
//! files it has, and an unchecked box `Delete branch <name>` while the branch exists, warned
//! when it is `not merged`. A branch checked out in another worktree cannot be checked, nor
//! one a worktree whose folder is gone still holds, and a
//! Worktree session's branch only together with its worktree: unchecking the worktree
//! unchecks the branch. The sheet reads its blocks from the core on every redraw, so git's
//! answers appear as they arrive; a block git has not answered says `checking…`, one whose
//! repository is busy with another branch change says so, neither offers a box, and a block
//! with nothing left to remove is hidden. Deleting while blocks are still checking removes
//! nothing for them. The removal runs in the background after the sheet closed; the project
//! row says `cleaning up…` meanwhile and the error bar tells what failed.
//!
//! The confirm button acts and closes the sheet; `Cancel`, Escape or a click on the
//! backdrop closes the sheet and changes nothing. Enter confirms. While the sheet is open no
//! other key reaches the window. When the action fails the sheet stays open and says why.

use iced::widget::{
    Column, Id, Row, button, column, container, mouse_area, opaque, rich_text, row, span,
};
use iced::widget::{space, stack, text};
use iced::{Alignment, Border, Element, Length, Padding, Task, Theme};
use partitur_core::{App, Cleanup, CleanupView, SessionId, SessionSummary, Status};

use crate::app::{BareKey, Message, State, report};
use crate::branch_controls::{CHECKBOX_SIZE, CHECKBOX_SPACING, fetch_checkbox};
use crate::ellipsis::keep_end;
use crate::theme::Tokens;
use crate::{badge, home_path, projects_view, style, theme};

/// The width of the sheet, in logical pixels.
pub(crate) const WIDTH: f32 = 560.0;

/// The room the sheet keeps around its content: above and below, then left and right.
pub(crate) const PADDING: [f32; 2] = [20.0, 22.0];

/// The space between the parts of the sheet.
pub(crate) const GAP: f32 = 16.0;

/// How many characters of a worktree's path the delete sheet shows, its end kept.
const PATH_BUDGET: usize = 56;

/// The corner radius of the sheet.
pub(crate) const RADIUS: f32 = 10.0;

/// The size of the sheet's title.
pub(crate) const TITLE_SIZE: f32 = 15.0;

/// The room the box that tells what the action means keeps around its text.
pub(crate) const BOX_PADDING: [f32; 2] = [10.0, 12.0];

/// The corner radius of the box that tells what the action means.
pub(crate) const BOX_RADIUS: f32 = 6.0;

/// The line height of the box that tells what the action means, in logical pixels.
pub(crate) const BOX_LINE_HEIGHT: f32 = 18.0;

/// How much of the window's colour the backdrop lays over the page.
pub(crate) const BACKDROP_STRENGTH: f32 = 0.72;

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
    /// What the user checked on each cleanup line of the summary, in its order; nothing at
    /// first.
    pub(crate) cleanup: Vec<Cleanup>,
}

/// An interaction with the sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmMessage {
    /// The user confirmed: the session is archived or deleted.
    Confirm,
    /// The user cancelled: the sheet closes.
    Cancel,
    /// The user checked or unchecked the worktree box of the cleanup line at the index.
    RemoveWorktree(usize, bool),
    /// The user checked or unchecked the branch box of the cleanup line at the index.
    DeleteBranch(usize, bool),
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
        let cleanup = summary
            .cleanup
            .iter()
            .map(|line| Cleanup {
                session: line.session.clone(),
                remove_worktree: false,
                delete_branch: false,
            })
            .collect();
        state.confirm = Some(ConfirmSheet {
            action,
            summary,
            archived,
            cleanup,
        });
    }
    Task::none()
}

/// Applies `message` to the open sheet. Confirming archives or deletes the session, with
/// the removal of what its boxes check, and closes the sheet, or keeps it open with the
/// error when that fails; cancelling closes it. A box changes only while the sheet offers
/// it, and unchecking a worktree unchecks its branch. Nothing happens while the sheet is
/// closed.
pub fn update(state: &mut State, message: ConfirmMessage) -> Task<Message> {
    let Some(sheet) = state.confirm.as_ref() else {
        return Task::none();
    };
    match message {
        ConfirmMessage::Confirm => {
            let session = sheet.summary.id.clone();
            let done = match sheet.action {
                Action::Archive => projects_view::archive(state, session),
                Action::Delete => {
                    let cleanup = sheet.cleanup.clone();
                    projects_view::delete(state, session, &cleanup)
                }
            };
            if done {
                state.confirm = None;
            }
        }
        ConfirmMessage::Cancel => {
            state.confirm = None;
            state.error = None;
        }
        ConfirmMessage::RemoveWorktree(index, checked) => check(state, index, |line, boxes| {
            if line.worktree.is_some() {
                boxes.remove_worktree = checked;
                boxes.delete_branch &= checked;
            }
        }),
        ConfirmMessage::DeleteBranch(index, checked) => check(state, index, |line, boxes| {
            if line.branch.is_some()
                && (!checked || branch_blocked(line, boxes.remove_worktree).is_none())
            {
                boxes.delete_branch = checked;
            }
        }),
    }
    Task::none()
}

/// Lets `set` change the boxes of the cleanup line at `index`, given the line as the core
/// has it now, when the line offers boxes.
fn check(state: &mut State, index: usize, set: impl FnOnce(&CleanupView, &mut Cleanup)) {
    let Some(sheet) = state.confirm.as_mut() else {
        return;
    };
    let line = lines(&state.app, sheet).into_iter().nth(index);
    if let Some(line) = line.filter(offers)
        && let Some(boxes) = sheet.cleanup.get_mut(index)
    {
        set(&line, boxes);
    }
}

/// The cleanup lines of `sheet` as far as git has answered, read from the core without
/// asking git; none for an archive sheet.
pub(crate) fn lines(app: &App, sheet: &ConfirmSheet) -> Vec<CleanupView> {
    match sheet.action {
        Action::Delete => app.cleanup(&sheet.summary.id),
        Action::Archive => Vec::new(),
    }
}

/// Whether `line` offers boxes: git has answered and its repository is not busy.
fn offers(line: &CleanupView) -> bool {
    !line.loading && !line.busy
}

/// Why the branch box of `line` cannot be checked while its worktree box is `removing` or
/// not, `None` when it can or there is no branch.
fn branch_blocked(line: &CleanupView, removing: bool) -> Option<String> {
    let branch = line.branch.as_ref()?;
    if let Some(at) = &branch.checked_out {
        return Some(format!("checked out in {}", at.display()));
    }
    if branch.prunable {
        return Some("its worktree folder is gone; run git worktree prune".to_owned());
    }
    if line.worktree.is_none() || removing {
        return None;
    }
    Some(if branch.in_own_worktree {
        "checked out in its worktree; remove the worktree too".to_owned()
    } else {
        "goes only with its worktree; remove the worktree too".to_owned()
    })
}

/// The warning about the uncommitted files of a worktree that has `changes` of them, `None`
/// when it has none.
fn changes_warning(changes: Option<usize>) -> Option<String> {
    match changes {
        Some(0) => None,
        Some(1) => Some("1 uncommitted file".to_owned()),
        Some(count) => Some(format!("{count} uncommitted files")),
        None => Some("uncommitted files unknown".to_owned()),
    }
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

/// `page` with `sheet` over it, with its cleanup `lines` and the error of a failed action in
/// the sheet when there is one. The backdrop takes every click meant for the page; a click
/// on it cancels.
pub fn view<'a>(
    page: Element<'a, Message>,
    sheet: &ConfirmSheet,
    lines: &[CleanupView],
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
    if let Some(cleanup) = cleanup(sheet, lines) {
        content = content.push(cleanup);
    }
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
        .style(sheet_style)
        .into();
    modal(page, card, Message::Confirm(ConfirmMessage::Cancel))
}

/// `page` under a dimming backdrop with `card` centred over it. The backdrop takes every
/// click meant for the page and sends `on_backdrop`; the card is opaque to clicks, so only a
/// click beside it reaches the backdrop.
pub(crate) fn modal<'a>(
    page: Element<'a, Message>,
    card: Element<'a, Message>,
    on_backdrop: Message,
) -> Element<'a, Message> {
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
    .on_press(on_backdrop);
    stack![page, opaque(backdrop)].into()
}

/// The blocks of the cleanup `lines` of `sheet` with something to show, one under the
/// other; `None` when none has.
fn cleanup<'a>(sheet: &ConfirmSheet, lines: &[CleanupView]) -> Option<Element<'a, Message>> {
    let blocks: Vec<Element<'a, Message>> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| cleanup_block(index, line, sheet.cleanup.get(index)))
        .collect();
    (!blocks.is_empty()).then(|| Column::from_vec(blocks).spacing(12).into())
}

/// The block of the cleanup `line` at `index`, with the `boxes` checked on it: its heading,
/// then its worktree and branch boxes with their warnings. While git has not answered or
/// the repository is busy the heading says so and there are no boxes; `None` when there is
/// nothing left to remove.
fn cleanup_block<'a>(
    index: usize,
    line: &CleanupView,
    boxes: Option<&Cleanup>,
) -> Option<Element<'a, Message>> {
    let heading = || {
        text(format!("{} · {}", line.repository, line.title))
            .size(theme::SMALL)
            .font(theme::UI_SEMIBOLD)
    };
    if line.loading {
        return Some(note(heading(), "checking…".to_owned()));
    }
    if line.busy {
        let busy = format!("{} is busy with another branch change", line.repository);
        return Some(note(heading(), busy));
    }
    if line.worktree.is_none() && line.branch.is_none() {
        return None;
    }
    let removing = boxes.is_some_and(|boxes| boxes.remove_worktree);
    let mut block = column![heading()].spacing(6);
    if let Some(worktree) = &line.worktree {
        let home = std::env::home_dir();
        let path = keep_end(
            &home_path::abbreviated(&worktree.path, home.as_deref()),
            PATH_BUDGET,
        );
        let toggle = fetch_checkbox(
            format!("Remove worktree {path}"),
            removing,
            Id::from(format!("delete-worktree-{index}")),
            Some(move |checked| Message::Confirm(ConfirmMessage::RemoveWorktree(index, checked))),
        );
        block = block.push(with_notes(toggle, changes_warning(worktree.changes), None));
    }
    if let Some(branch) = &line.branch {
        let blocked = branch_blocked(line, removing);
        let on_toggle = blocked.is_none().then_some(move |checked| {
            Message::Confirm(ConfirmMessage::DeleteBranch(index, checked))
        });
        let toggle = fetch_checkbox(
            format!("Delete branch {}", branch.name),
            boxes.is_some_and(|boxes| boxes.delete_branch),
            Id::from(format!("delete-branch-{index}")),
            on_toggle,
        );
        let warning = (!branch.merged).then(|| "not merged".to_owned());
        block = block.push(with_notes(toggle, warning, blocked));
    }
    Some(block.into())
}

/// `heading` with the dim `note` beside it.
fn note(heading: text::Text<'_>, note: String) -> Element<'_, Message> {
    row![heading, text(note).size(theme::SMALL).style(style::dim)]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

/// The checkbox `toggle` over the `warning`, in the colour of a session that asks for the
/// user, and the dim `reason` it cannot be checked, both in line with its label.
fn with_notes(
    toggle: Element<'_, Message>,
    warning: Option<String>,
    reason: Option<String>,
) -> Element<'_, Message> {
    if warning.is_none() && reason.is_none() {
        return toggle;
    }
    let mut notes = Row::new().spacing(8);
    if let Some(warning) = warning {
        notes = notes.push(
            text(warning)
                .size(theme::SMALL)
                .style(style::ink(|tokens| tokens.asks_you.fg)),
        );
    }
    if let Some(reason) = reason {
        notes = notes.push(text(reason).size(theme::SMALL).style(style::dim));
    }
    let indent = CHECKBOX_SIZE + CHECKBOX_SPACING;
    column![toggle, container(notes).padding(Padding::ZERO.left(indent))]
        .spacing(2)
        .into()
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
        Status::Interrupted => "interrupted",
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
pub(crate) fn meaning_box(content: Element<'_, Message>) -> Element<'_, Message> {
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
pub(crate) fn sheet_style(theme: &Theme) -> iced::widget::container::Style {
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
    use std::path::PathBuf;

    use partitur_core::testing::{
        GitCall, GitOp, MemoryGit, MemoryStore, RecordingBackend, repo_info, single_claude,
    };
    use partitur_core::{
        App, BackendEvent, GlobalSettings, Head, RepositoryId, Scope, StopReason, Store as _,
        Update, Workspace, WorktreeInfo,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::app::{Tab, boot, update as app_update, view as app_view};
    use crate::projects_view::ProjectsMessage;
    use crate::test_support::{attach, deliver, orchestrated, settle_git};

    /// A state whose only project holds one session that finished its first turn, titled by
    /// its prompt `title`, with the backend, the store and the folder, which must outlive the
    /// state.
    fn finished(prompt: &str) -> (State, SessionId, MemoryStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let backend = RecordingBackend::new();
        let store = MemoryStore::new();
        let (backends, default) = single_claude();
        let app = App::load(
            Box::new(backend.clone()),
            Box::new(store.clone()),
            Box::new(MemoryGit::new()),
            backends,
            default,
            GlobalSettings::default(),
        )
        .unwrap();
        let mut state = boot(app, mpsc::unbounded().1, mpsc::unbounded().1).0;
        let project = state.app.open_project(dir.path()).unwrap();
        let session = state.app.new_session(project, Scope::Project).unwrap();
        attach(&mut state, &backend, &session, "b");
        state.app.submit_prompt(prompt, Vec::new()).unwrap();
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
        ui.find("Archive “untitled”").unwrap();
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
        ui.find("Delete “untitled”").unwrap();
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
        ui.find("Delete “untitled”").unwrap();
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

    /// A project with the repositories `api` and `web` whose sessions make branches, after a
    /// restart, with the git that answers for them and the folder, which must outlive the
    /// state.
    ///
    /// The orchestrator `Add login` started the child `login` in `api`, working in a
    /// worktree at `<project>/wt/feat-x` on the branch `feat/x` from `main`; the worktree has
    /// three uncommitted files and the branch is not merged. The top-level session `signup`
    /// works in `api` on the new branch `feat/y`, which is merged.
    struct Cleaning {
        state: State,
        git: MemoryGit,
        orchestrator: SessionId,
        child: SessionId,
        signup: SessionId,
        api: PathBuf,
        wt: PathBuf,
        _dir: TempDir,
    }

    fn cleaning() -> Cleaning {
        let dir = TempDir::new().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        for name in ["api", "web"] {
            std::fs::create_dir_all(root.join(name).join(".git")).unwrap();
        }
        let (api, wt) = (root.join("api"), root.join("wt").join("feat-x"));
        let store = MemoryStore::new();
        let (backends, default) = single_claude();
        let mut app = App::load(
            Box::new(RecordingBackend::new()),
            Box::new(store.clone()),
            Box::new(MemoryGit::new()),
            backends.clone(),
            default.clone(),
            GlobalSettings::default(),
        )
        .unwrap();
        let project = app.open_project(&root).unwrap();
        let in_api = Scope::Repository(RepositoryId::try_from("api").unwrap());
        let orchestrator = app.new_session(project.clone(), Scope::Project).unwrap();
        let child = app.new_session(project.clone(), in_api.clone()).unwrap();
        let signup = app.new_session(project.clone(), in_api).unwrap();
        drop(app);
        let mut sessions = store.load_sessions(project.clone()).unwrap();
        for session in &mut sessions {
            if session.id == orchestrator {
                session.title = "Add login".into();
                session.orchestrator = true;
            } else if session.id == child {
                session.title = "login".into();
                session.parent = Some(orchestrator.clone());
                session.workspace = Workspace::Worktree {
                    branch: "feat/x".into(),
                    base: "main".into(),
                    fetched: false,
                    path: wt.clone(),
                };
            } else {
                session.title = "signup".into();
                session.workspace = Workspace::NewBranch {
                    branch: "feat/y".into(),
                    base: "main".into(),
                    fetched: false,
                };
            }
        }
        store.save_sessions(project, &sessions).unwrap();

        let git = MemoryGit::new();
        let mut info = repo_info(&api, "main", &["main", "feat/x", "feat/y"], &[]);
        info.worktrees.push(WorktreeInfo {
            path: wt.clone(),
            head: Head::Branch("feat/x".into()),
            prunable: false,
        });
        git.add_repository(api.clone(), info);
        git.add_repository(
            root.join("web"),
            repo_info(&root.join("web"), "main", &["main"], &[]),
        );
        git.set_changes(&wt, 3);
        git.set_merged(&api, "feat/y", true);
        let app = App::load(
            Box::new(RecordingBackend::new()),
            Box::new(store),
            Box::new(git.clone()),
            backends,
            default,
            GlobalSettings::default(),
        )
        .unwrap();
        let mut state = boot(app, mpsc::unbounded().1, mpsc::unbounded().1).0;
        state.tab = Tab::Projects;
        Cleaning {
            state,
            git,
            orchestrator,
            child,
            signup,
            api,
            wt,
            _dir: dir,
        }
    }

    /// The worktree removals and branch deletions `git` was asked to run.
    fn removals(git: &MemoryGit) -> Vec<GitCall> {
        git.calls()
            .into_iter()
            .filter(|call| {
                matches!(
                    call,
                    GitCall::RemoveWorktree { .. } | GitCall::DeleteBranch { .. }
                )
            })
            .collect()
    }

    /// What the open sheet has checked.
    fn checked(state: &State) -> Vec<(bool, bool)> {
        state
            .confirm
            .as_ref()
            .unwrap()
            .cleanup
            .iter()
            .map(|boxes| (boxes.remove_worktree, boxes.delete_branch))
            .collect()
    }

    fn on_sheet(state: &mut State, message: ConfirmMessage) {
        send(state, Message::Confirm(message));
    }

    /// Whether the live tree still holds `session` at the top level.
    fn listed(state: &State, session: &SessionId) -> bool {
        state.app.tree().projects[0]
            .sessions
            .iter()
            .any(|top| &top.id == session)
    }

    #[test]
    fn the_delete_sheet_checks_in_the_background() {
        let mut fixture = cleaning();
        let orchestrator = fixture.orchestrator.clone();

        send(&mut fixture.state, ask(Action::Delete, orchestrator));

        {
            let mut ui = simulator(app_view(&fixture.state));
            ui.find("api · login").unwrap();
            ui.find("checking…").unwrap();
            assert!(ui.find(Id::new("delete-worktree-0")).is_err());
            assert!(ui.find(Id::new("delete-branch-0")).is_err());
        }

        let _task = settle_git(&mut fixture.state, &fixture.git);

        let mut ui = simulator(app_view(&fixture.state));
        assert!(ui.find("checking…").is_err());
        ui.find(format!("Remove worktree {}", fixture.wt.display()).as_str())
            .unwrap();
        ui.find("3 uncommitted files").unwrap();
        ui.find("Delete branch feat/x").unwrap();
        ui.find("not merged").unwrap();
        ui.find(Id::new("delete-worktree-0")).unwrap();
        ui.find(Id::new("delete-branch-0")).unwrap();
    }

    #[test]
    fn the_delete_sheet_offers_unchecked_boxes_with_their_warnings() {
        let mut fixture = cleaning();
        let orchestrator = fixture.orchestrator.clone();
        send(&mut fixture.state, ask(Action::Delete, orchestrator));
        let _task = settle_git(&mut fixture.state, &fixture.git);

        let sheet = fixture.state.confirm.as_ref().unwrap();
        assert_eq!(
            sheet.cleanup,
            [Cleanup {
                session: fixture.child.clone(),
                remove_worktree: false,
                delete_branch: false,
            }]
        );
        let mut ui = simulator(app_view(&fixture.state));
        ui.find("api · login").unwrap();
        ui.find("3 uncommitted files").unwrap();
        ui.find("not merged").unwrap();
        ui.find("checked out in its worktree; remove the worktree too")
            .unwrap();
        ui.click(Id::new("delete-worktree-0")).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(
            messages,
            [Message::Confirm(ConfirmMessage::RemoveWorktree(0, true))]
        );
    }

    #[test]
    fn the_warnings_count_the_uncommitted_files() {
        assert_eq!(changes_warning(Some(0)), None);
        assert_eq!(changes_warning(Some(1)).unwrap(), "1 uncommitted file");
        assert_eq!(changes_warning(Some(4)).unwrap(), "4 uncommitted files");
        assert_eq!(changes_warning(None).unwrap(), "uncommitted files unknown");
    }

    #[test]
    fn confirming_while_checking_removes_nothing() {
        let mut fixture = cleaning();
        let orchestrator = fixture.orchestrator.clone();
        send(
            &mut fixture.state,
            ask(Action::Delete, orchestrator.clone()),
        );

        on_sheet(&mut fixture.state, ConfirmMessage::RemoveWorktree(0, true));
        on_sheet(&mut fixture.state, ConfirmMessage::DeleteBranch(0, true));
        assert_eq!(checked(&fixture.state), [(false, false)]);
        send(&mut fixture.state, Message::Key(BareKey::Enter));

        assert_eq!(fixture.state.confirm, None);
        assert!(!listed(&fixture.state, &orchestrator));
        let _task = settle_git(&mut fixture.state, &fixture.git);
        assert_eq!(removals(&fixture.git), []);
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn confirming_queues_only_what_was_checked() {
        let mut fixture = cleaning();
        let orchestrator = fixture.orchestrator.clone();
        send(
            &mut fixture.state,
            ask(Action::Delete, orchestrator.clone()),
        );
        let _task = settle_git(&mut fixture.state, &fixture.git);

        on_sheet(&mut fixture.state, ConfirmMessage::RemoveWorktree(0, true));
        send(&mut fixture.state, Message::Key(BareKey::Enter));

        assert_eq!(fixture.state.confirm, None);
        assert!(!listed(&fixture.state, &orchestrator));
        assert!(
            simulator(app_view(&fixture.state))
                .find("cleaning up…")
                .is_ok()
        );
        let _task = settle_git(&mut fixture.state, &fixture.git);
        assert_eq!(
            removals(&fixture.git),
            [GitCall::RemoveWorktree {
                repository: fixture.api.clone(),
                path: fixture.wt.clone(),
            }]
        );
        assert_eq!(fixture.state.error, None);
        assert!(
            simulator(app_view(&fixture.state))
                .find("cleaning up…")
                .is_err()
        );
    }

    #[test]
    fn the_branch_of_a_worktree_can_go_only_with_its_worktree() {
        let mut fixture = cleaning();
        let orchestrator = fixture.orchestrator.clone();
        send(&mut fixture.state, ask(Action::Delete, orchestrator));
        let _task = settle_git(&mut fixture.state, &fixture.git);

        on_sheet(&mut fixture.state, ConfirmMessage::DeleteBranch(0, true));
        assert_eq!(checked(&fixture.state), [(false, false)]);
        {
            let mut ui = simulator(app_view(&fixture.state));
            ui.click(Id::new("delete-branch-0")).unwrap();
            assert_eq!(ui.into_messages().count(), 0);
        }

        on_sheet(&mut fixture.state, ConfirmMessage::RemoveWorktree(0, true));
        {
            let mut ui = simulator(app_view(&fixture.state));
            assert!(
                ui.find("checked out in its worktree; remove the worktree too")
                    .is_err()
            );
            ui.click(Id::new("delete-branch-0")).unwrap();
            let messages: Vec<Message> = ui.into_messages().collect();
            assert_eq!(
                messages,
                [Message::Confirm(ConfirmMessage::DeleteBranch(0, true))]
            );
        }
        on_sheet(&mut fixture.state, ConfirmMessage::DeleteBranch(0, true));
        assert_eq!(checked(&fixture.state), [(true, true)]);

        on_sheet(&mut fixture.state, ConfirmMessage::RemoveWorktree(0, false));
        assert_eq!(checked(&fixture.state), [(false, false)]);

        on_sheet(&mut fixture.state, ConfirmMessage::RemoveWorktree(0, true));
        on_sheet(&mut fixture.state, ConfirmMessage::DeleteBranch(0, true));
        send(&mut fixture.state, Message::Key(BareKey::Enter));
        let _task = settle_git(&mut fixture.state, &fixture.git);
        assert_eq!(
            removals(&fixture.git),
            [
                GitCall::RemoveWorktree {
                    repository: fixture.api.clone(),
                    path: fixture.wt.clone(),
                },
                GitCall::DeleteBranch {
                    repository: fixture.api.clone(),
                    branch: "feat/x".into(),
                },
            ]
        );
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn a_branch_checked_out_elsewhere_cannot_be_checked() {
        let mut fixture = cleaning();
        let other = fixture.wt.with_file_name("other");
        let mut info = fixture.git.repository(&fixture.api).unwrap();
        info.worktrees.push(WorktreeInfo {
            path: other.clone(),
            head: Head::Branch("feat/y".into()),
            prunable: false,
        });
        fixture.git.add_repository(fixture.api.clone(), info);
        let signup = fixture.signup.clone();
        send(&mut fixture.state, ask(Action::Delete, signup));
        let _task = settle_git(&mut fixture.state, &fixture.git);

        on_sheet(&mut fixture.state, ConfirmMessage::DeleteBranch(0, true));

        assert_eq!(checked(&fixture.state), [(false, false)]);
        let mut ui = simulator(app_view(&fixture.state));
        ui.find("Delete branch feat/y").unwrap();
        ui.find(format!("checked out in {}", other.display()).as_str())
            .unwrap();
        assert!(ui.find("not merged").is_err());
        assert!(ui.find(Id::new("delete-worktree-0")).is_err());
        ui.click(Id::new("delete-branch-0")).unwrap();
        assert_eq!(ui.into_messages().count(), 0);
    }

    #[test]
    fn a_branch_held_by_a_prunable_worktree_cannot_be_checked() {
        let mut fixture = cleaning();
        let gone = fixture.wt.with_file_name("gone");
        let mut info = fixture.git.repository(&fixture.api).unwrap();
        info.worktrees.push(WorktreeInfo {
            path: gone,
            head: Head::Branch("feat/y".into()),
            prunable: true,
        });
        fixture.git.add_repository(fixture.api.clone(), info);
        let signup = fixture.signup.clone();
        send(&mut fixture.state, ask(Action::Delete, signup));
        let _task = settle_git(&mut fixture.state, &fixture.git);

        on_sheet(&mut fixture.state, ConfirmMessage::DeleteBranch(0, true));

        assert_eq!(checked(&fixture.state), [(false, false)]);
        let mut ui = simulator(app_view(&fixture.state));
        ui.find("Delete branch feat/y").unwrap();
        ui.find("its worktree folder is gone; run git worktree prune")
            .unwrap();
    }

    #[test]
    fn a_line_with_nothing_left_is_hidden() {
        let mut fixture = cleaning();
        let mut info = fixture.git.repository(&fixture.api).unwrap();
        info.branches.retain(|branch| branch.name != "feat/y");
        fixture.git.add_repository(fixture.api.clone(), info);
        let signup = fixture.signup.clone();
        send(&mut fixture.state, ask(Action::Delete, signup));
        let _task = settle_git(&mut fixture.state, &fixture.git);

        let mut ui = simulator(app_view(&fixture.state));
        ui.find("Delete “signup”").unwrap();
        assert!(ui.find("api · signup").is_err());
    }

    #[test]
    fn a_new_branch_can_be_deleted_on_its_own() {
        let mut fixture = cleaning();
        let signup = fixture.signup.clone();
        send(&mut fixture.state, ask(Action::Delete, signup));
        let _task = settle_git(&mut fixture.state, &fixture.git);

        on_sheet(&mut fixture.state, ConfirmMessage::DeleteBranch(0, true));
        assert_eq!(checked(&fixture.state), [(false, true)]);
        on_sheet(&mut fixture.state, ConfirmMessage::DeleteBranch(0, false));
        assert_eq!(checked(&fixture.state), [(false, false)]);
    }

    #[test]
    fn a_failed_removal_shows_in_the_error_bar() {
        let mut fixture = cleaning();
        let orchestrator = fixture.orchestrator.clone();
        send(
            &mut fixture.state,
            ask(Action::Delete, orchestrator.clone()),
        );
        let _task = settle_git(&mut fixture.state, &fixture.git);
        fixture.git.fail_next(GitOp::RemoveWorktree, &fixture.api);

        on_sheet(&mut fixture.state, ConfirmMessage::RemoveWorktree(0, true));
        send(&mut fixture.state, Message::Key(BareKey::Enter));
        let _task = settle_git(&mut fixture.state, &fixture.git);

        assert!(!listed(&fixture.state, &orchestrator));
        let error = fixture.state.error.clone().unwrap();
        assert!(
            error.starts_with("could not remove the worktree or branch in api: "),
            "{error}"
        );
        let mut ui = simulator(app_view(&fixture.state));
        assert_eq!(crate::test_support::shown(&mut ui, &error), 1);
    }

    #[test]
    fn a_busy_repository_shows_its_reason_and_no_boxes() {
        let mut fixture = cleaning();
        let orchestrator = fixture.orchestrator.clone();
        send(&mut fixture.state, ask(Action::Delete, orchestrator));
        let _task = settle_git(&mut fixture.state, &fixture.git);
        on_sheet(&mut fixture.state, ConfirmMessage::RemoveWorktree(0, true));
        send(&mut fixture.state, Message::Key(BareKey::Enter));

        // The removal holds `api` while git answers the reading of the next sheet.
        let signup = fixture.signup.clone();
        send(&mut fixture.state, ask(Action::Delete, signup));
        for event in fixture.git.answer() {
            send(&mut fixture.state, Message::Git(event));
        }

        on_sheet(&mut fixture.state, ConfirmMessage::DeleteBranch(0, true));
        assert_eq!(checked(&fixture.state), [(false, false)]);
        {
            let mut ui = simulator(app_view(&fixture.state));
            ui.find("api · signup").unwrap();
            ui.find("api is busy with another branch change").unwrap();
            assert!(ui.find(Id::new("delete-branch-0")).is_err());
        }

        let _task = settle_git(&mut fixture.state, &fixture.git);
        let mut ui = simulator(app_view(&fixture.state));
        ui.find(Id::new("delete-branch-0")).unwrap();
    }

    #[test]
    fn the_archive_sheet_shows_no_boxes() {
        let mut fixture = cleaning();
        let orchestrator = fixture.orchestrator.clone();
        send(
            &mut fixture.state,
            ask(Action::Delete, orchestrator.clone()),
        );
        let _task = settle_git(&mut fixture.state, &fixture.git);
        on_sheet(&mut fixture.state, ConfirmMessage::Cancel);

        send(&mut fixture.state, ask(Action::Archive, orchestrator));

        assert_eq!(fixture.state.confirm.as_ref().unwrap().cleanup, []);
        on_sheet(&mut fixture.state, ConfirmMessage::RemoveWorktree(0, true));
        let mut ui = simulator(app_view(&fixture.state));
        ui.find("Archive “Add login”").unwrap();
        assert!(ui.find("api · login").is_err());
        assert!(ui.find(Id::new("delete-worktree-0")).is_err());
    }

    #[test]
    fn a_closed_sheet_ignores_its_messages() {
        let (mut state, _session, _store, _dir) = finished("Fix it");

        send(&mut state, Message::Confirm(ConfirmMessage::Confirm));

        assert_eq!(archived(&state), 0);
        assert_eq!(top_level(&state), 1);
    }
}
