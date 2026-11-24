//! The Projects tab: the tree of open projects with their sessions, over a status line.
//!
//! The tree scrolls in one column, at most 1080 pixels wide and centred in a wider window,
//! with 40 pixels on either side in a narrower one; the status line stays at the bottom
//! across the whole width. A project starts with its header: the chevron that folds the
//! project away under its header and opens it again (the core remembers which), its name,
//! how many repositories it has, its folder, the instruction files (`AGENTS.md`,
//! `CLAUDE.md`) its folder holds and a dim `cleaning up…` while a worktree or branch a delete
//! checked for removal is being removed in it. While the pointer is over the header it takes
//! the panel colour and shows the close button on the right. Under an open header come its
//! sessions as rows in fixed columns, their titles under the project's name: a chevron when
//! sessions were spawned from the session (it hides and shows them, and the core remembers
//! which), the title cell, the status pill (an outlined `idle` while idle), the context
//! usage while the session runs, waits or is idle and its usage is known, and how long ago
//! it was active (`now`, `4 min`, `2 h`, `yesterday`, `3 d`). The title cell holds the skill
//! the first prompt started with as a chip, the title on one line, the repository the
//! session works in, a pill for an orchestrator and a pill when a session below waits for
//! the user, clipped when they outgrow the cell; the title's tooltip is the first prompt,
//! cut to its first lines so it stays under the row.
//! A spawned session sits one level deeper than its parent and leads with its repository.
//! An orchestrator starts folded: under it a row sums up its children,
//! `2 children · api running · web idle`. A click anywhere on a row but on its chevron opens
//! its session. F2 on the cursor's row turns the title into a field, where Enter keeps the
//! typed title and Escape the old one; picking another row, changing the tab or hiding the
//! row closes the field too. One row is highlighted: the
//! cursor, which follows the selected session and which the arrow keys move over the rows
//! that are shown. The highlighted row and the row under the pointer show an action strip
//! at the end of their title cell, where it moves none of the columns, with the archive and
//! the delete button, which read `Stop and archive` and `Stop and delete` while the session
//! runs or waits and cannot be pressed on a child session, which goes with its
//! orchestrator. A project's rows end with one `new session` row, which opens the
//! new-session sheet where the repository is picked; for a project with several
//! repositories it adds the hint `orchestrator or a single repo — chosen in the sheet`.
//! Below it, a project with archived sessions has an `archived · N` row, collapsed at
//! first, that shows and hides them: each with its title and when it was archived, the
//! sessions archived with it one level deeper while its chevron shows them, else the row
//! that sums them up. Only while the pointer is over the row of a session archived on its
//! own account does it end in a strip like a live row's, with the restore and the delete
//! button. A folder being opened as a project follows the projects as a header of its own,
//! with its name, its folder and a dim `scanning…`, until its scan and settings have arrived;
//! then it is a project like the others, or, when it could not be opened, the error bar says
//! why. The tree's last row opens another project folder.
//!
//! Deleting a session moves the cursor to the nearest row still shown and forgets what the
//! interface remembered about the deleted sessions; when the Session, Spec or Env tab showed
//! one of them, the Projects tab is shown instead. Folding a project takes the cursor off its
//! rows; N then starts a session in that project until a row has the cursor again. Picking
//! a session elsewhere (the jump panel, a card) opens its project when it is folded.
//!
//! The status line counts the live sessions that run, that wait for the user, that were
//! interrupted and that failed, each with its dot, then those that are idle and done, joined
//! by dots, and, dim, the archived
//! ones, leaving out every count of none. On the right it names the keys `Enter`, `G`, `N` and `A`.
//!
//! A child at a gate or with a question whose orchestrator decides for it does not wait for
//! the user: its pill reads `waits for orchestrator` in muted letters, its step dot is dim,
//! it marks none of the sessions above it, and neither `N gates waiting`, `asks you` nor `G`
//! counts it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use iced::widget::text::Wrapping;
use iced::widget::{
    Column, Row, Space, button, column, container, mouse_area, row, scrollable, space, text,
    text_input, tooltip,
};
use iced::{Alignment, Color, Element, Length, Padding, Task};
use partitur_core::{
    Cleanup, Fold, OpeningNode, ProjectId, ProjectNode, Scope, SessionId, SessionNode, Status,
    StepCell, TreeModel, age_label, day_month,
};

use crate::app::{self, Message, State, Tab, report};
use crate::confirm_sheet::{self, Action};
use crate::ellipsis::{keep_end, keep_head, keep_start};
use crate::icon::{self, Icon, icon};
use crate::status_pills::CHILD_ASKS_YOU;
use crate::theme::Tokens;
use crate::{badge, home_path, new_session_sheet, session_view, style, theme, tokens_label};

/// The height of a row of the tree, in logical pixels.
const ROW_HEIGHT: f32 = 28.0;

/// The vertical space between the rows of one project.
const ROW_GAP: f32 = 2.0;

/// The vertical space between one project and the next.
const PROJECT_GAP: f32 = 22.0;

/// The space above and below the tree.
const TREE_MARGIN_Y: f32 = 28.0;

/// The space between the window's edge and the tree's text, left and right, while the
/// window is too narrow to centre the tree.
const TREE_MARGIN_X: f32 = 40.0;

/// The widest the tree gets, row backgrounds included; a wider window centres it.
pub(crate) const CONTENT_WIDTH: f32 = 1080.0;

/// How far a row's background reaches beyond the tree's text edge on each side, so the
/// highlighted row frames its text instead of touching it.
const BLEED: f32 = 12.0;

/// How far the `new session` row and the archived sessions' row sit right of the project
/// header, which puts their icons under the titles of the top-level sessions.
const SESSION_INDENT: f32 = icon::SIZE + PART_GAP;

/// How far each level of spawned sessions sits right of its parent.
const LEVEL_INDENT: f32 = 32.0;

/// How far the slot at the left of a row reaches beyond its icon on each side. The slot is
/// what a click on a chevron has to hit, and the icon alone would be too small a target.
const SLOT_REACH: f32 = 4.0;

/// The width of the chevron column of a session row, which is the width of its icon.
const CHEVRON_COLUMN: f32 = 12.0;

/// The width of the slot at the left of a row: the chevron column and its reach on both
/// sides.
const SLOT_WIDTH: f32 = CHEVRON_COLUMN + 2.0 * SLOT_REACH;

/// The width of a session row's status column, which holds its pill on the left. It fits
/// `12 background tasks` on one line.
const STATUS_COLUMN: f32 = 128.0;

/// The width of a session row's step column, which holds the step its workflow stands at.
const STEP_COLUMN: f32 = 150.0;

/// The size of the dot before a step's label.
const STEP_DOT: f32 = 6.0;

/// The width of a session row's context column, right-aligned.
const CONTEXT_COLUMN: f32 = 80.0;

/// The width of a session row's activity column, right-aligned.
const ACTIVITY_COLUMN: f32 = 64.0;

/// The space between the columns of a session row.
const COLUMN_GAP: f32 = 12.0;

/// The space between a session's skill chip and its title.
const SKILL_GAP: f32 = 8.0;

/// The height of the row that sums up a folded orchestrator's children.
const SUMMARY_HEIGHT: f32 = 26.0;

/// The widest the tooltip with a session's first prompt gets before it wraps.
const PROMPT_TIP_WIDTH: f32 = 480.0;

/// The most lines of a session's first prompt its tooltip shows. A taller tooltip no
/// longer fits under the row, and iced moves it to the top of the window.
const PROMPT_TIP_LINES: usize = 12;

/// The most characters of a session's first prompt its tooltip shows, so wrapped lines
/// keep it short too.
const PROMPT_TIP_BUDGET: usize = 600;

/// The space between the parts of a session row.
const PART_GAP: f32 = 10.0;

/// The space between the parts of a project header.
const HEADER_GAP: f32 = 12.0;

/// The height of a project header, in logical pixels.
const HEADER_HEIGHT: f32 = 30.0;

/// The side of the close button, in logical pixels.
const CLOSE_SIZE: f32 = 22.0;

/// The padding around the close icon, which makes its button [`CLOSE_SIZE`] square.
const CLOSE_PADDING: f32 = (CLOSE_SIZE - icon::SIZE) / 2.0;

/// The height of the status line with the line above it, in logical pixels.
const STATUS_HEIGHT: f32 = 32.0;

/// The most characters of a session's title a row shows. Titles named by an orchestrator
/// have no limit of their own, and a row has one line.
const TITLE_BUDGET: usize = 60;

/// The most characters of a project's folder its header shows; a longer path keeps its end.
const PATH_BUDGET: usize = 48;

/// The extra space above the row of a project's archived sessions, which sets them apart from
/// the rows that start sessions.
const ARCHIVED_GAP: f32 = 4.0;

/// Why a child session's archive button cannot be pressed.
const ARCHIVE_WITH_ORCHESTRATOR: &str = "archive the orchestrator";

/// Why a child session's delete button cannot be pressed.
const DELETE_WITH_ORCHESTRATOR: &str = "delete the orchestrator";

/// What the `new session` row of a project with several repositories adds after its label.
const NEW_SESSION_HINT: &str = "orchestrator or a single repo — chosen in the sheet";

/// The keys the status line names, each with what it does on this tab.
const KEY_HINTS: [(&str, &str); 4] = [
    ("Enter", "open"),
    ("G", "next gate"),
    ("N", "new session"),
    ("A", "archive"),
];

/// A colour picked from the active theme's tokens.
type Ink = fn(&Tokens) -> Color;

/// What the Projects tab remembers between frames. At start the application puts the
/// cursor on its current session; which sessions show their children the core remembers.
#[derive(Debug, Default)]
pub struct ProjectsState {
    /// The session of the highlighted row, which the arrow keys move and Enter opens.
    pub(crate) cursor: Option<SessionId>,
    /// The project whose folding took the cursor off its rows, while no row has the cursor:
    /// N starts a session there.
    pub(crate) fallback_project: Option<ProjectId>,
    /// The session whose title is being edited, with the text typed so far.
    pub(crate) renaming: Option<(SessionId, String)>,
    /// The projects whose archived sessions are shown; every other project hides them.
    pub(crate) archived_open: HashSet<ProjectId>,
    /// The session, live or archived, whose row the pointer is over.
    pub(crate) hovered: Option<SessionId>,
    /// The project whose header the pointer is over, which shows its close button.
    pub(crate) hovered_project: Option<ProjectId>,
}

/// An interaction on the Projects tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectsMessage {
    /// The user asked to open a project folder; the folder dialog opens.
    OpenFolder,
    /// The folder dialog closed: with the chosen folder, or `None` when it was cancelled.
    FolderPicked(Option<PathBuf>),
    /// The user asked for a new session in a project, for the given scope: the new-session
    /// sheet opens.
    NewSession(ProjectId, Scope),
    /// The user closed a project.
    Close(ProjectId),
    /// The user folded a project's rows away under its header, or opened them again.
    ToggleProject(ProjectId),
    /// The pointer entered a project's header.
    HoverProject(ProjectId),
    /// The pointer left a project's header.
    UnhoverProject(ProjectId),
    /// The user hid or showed the sessions spawned from a session; the core remembers which.
    ToggleChildren(SessionId),
    /// The user pressed F2 on a session's row: the title turns into a field that holds it.
    StartRename(SessionId),
    /// The user typed in the title field.
    RenameInput(String),
    /// The user pressed Enter in the title field: the session takes the typed title, unless
    /// it is blank.
    CommitRename,
    /// The user pressed Escape in the title field: the title stays as it was.
    CancelRename,
    /// The user hid or showed a project's archived sessions.
    ToggleArchived(ProjectId),
    /// The user restored an archived session.
    Restore(SessionId),
    /// The user asked to archive a top-level session: the sheet that confirms it opens.
    Archive(SessionId),
    /// The user asked to delete a top-level session, live or archived: the sheet that
    /// confirms it opens.
    Delete(SessionId),
    /// The pointer entered the row of a session.
    Hover(SessionId),
    /// The pointer left the row of a session.
    Unhover(SessionId),
    /// The user pressed A: the archive sheet opens for the cursor's session, when it is a
    /// live top-level one.
    ArchiveAtCursor,
    /// The user pressed D: the delete sheet opens for the cursor's session, when it is a
    /// top-level one, live or archived.
    DeleteAtCursor,
    /// The user pressed the down arrow: the cursor moves to the next visible session.
    CursorDown,
    /// The user pressed the up arrow: the cursor moves to the previous visible session.
    CursorUp,
    /// The user pressed Enter: the cursor's session is picked.
    OpenCursor,
    /// The user pressed N: the new-session sheet opens for a session of the whole project
    /// in the cursor's project.
    NewSessionAtCursor,
}

/// The Projects tab: every project of `tree` with its sessions and repositories, drawn with
/// the cursor and the title field of `state` and each session's activity told as of `now`
/// (seconds since the Unix epoch), over the status line.
///
/// The element owns what it shows, so it does not borrow `tree`, which the caller builds
/// afresh for every frame.
pub fn view<'a>(tree: &TreeModel, state: &ProjectsState, now: u64) -> Element<'a, Message> {
    let home = std::env::home_dir();
    let mut content = Column::new()
        .spacing(PROJECT_GAP)
        .width(Length::Fill)
        .max_width(CONTENT_WIDTH);
    if tree.projects.is_empty() && tree.opening.is_empty() {
        content = content
            .push(container(text("No projects open").style(style::muted)).padding([0.0, BLEED]));
    }
    for project in &tree.projects {
        content = content.push(project_section(project, state, home.as_deref(), now));
    }
    for opening in &tree.opening {
        content = content.push(opening_header(opening, home.as_deref()));
    }
    content = content.push(open_folder_row());
    // The rows' backgrounds reach `BLEED` beyond the text edge, so the tree keeps that much
    // less margin and every row pads its content by it.
    let centred = container(content)
        .center_x(Length::Fill)
        .padding([TREE_MARGIN_Y, TREE_MARGIN_X - BLEED]);
    column![
        scrollable(centred).width(Length::Fill).height(Length::Fill),
        status_line(tree),
    ]
    .into()
}

/// Applies `message` to the Projects tab and returns the follow-up work.
///
/// Opening, selecting and closing go through the core [`App`](partitur_core::App); when it
/// refuses, the error bar shows why. Selecting a session switches to the Session tab, where
/// the session is now current, moves the cursor onto it and unfolds the sessions above it;
/// closing a project forgets what the interface remembers about its sessions. Asking for a
/// new session opens the new-session sheet, and archiving and deleting open the sheet that
/// confirms them. Renaming turns a title into a field with the focus and its text selected;
/// committing hands the typed title to the core, which ignores a blank one. The key presses
/// (moving the cursor, opening its session, asking for a session in its project, archiving or
/// deleting its session) count only while the Projects tab is shown. Once a row has the
/// cursor, the project whose folding took the cursor away is forgotten. The title field
/// closes, keeping the old title, when another row is highlighted or picked, or when its row
/// is no longer shown.
pub fn update(state: &mut State, message: ProjectsMessage) -> Task<Message> {
    let task = dispatch(state, message);
    settle(state);
    task
}

/// Picks a session, from any tab: the title field closes and the Session tab shows it. The
/// rest is as for [`update`].
pub fn select(state: &mut State, session: SessionId) -> Task<Message> {
    pick(state, session);
    settle(state);
    Task::none()
}

/// Makes the session current and shows it, after the title field closed.
fn pick(state: &mut State, session: SessionId) {
    state.projects.renaming = None;
    if report(&mut state.error, state.app.select_session(session)).is_some() {
        show_current_session(state);
    }
}

/// Drops what a change leaves behind: the fallback project once a row has the cursor, and a
/// title field whose row is no longer shown.
fn settle(state: &mut State) {
    if state.projects.cursor.is_some() {
        state.projects.fallback_project = None;
    }
    if let Some((renamed, _)) = &state.projects.renaming
        && !state
            .app
            .tree()
            .visible_sessions(&state.projects.archived_open)
            .contains(renamed)
    {
        state.projects.renaming = None;
    }
}

/// Closes the title field and hands what it holds to the core as the session's title.
fn commit_rename(state: &mut State) {
    if let Some((session, title)) = state.projects.renaming.take() {
        report(&mut state.error, state.app.rename_session(session, &title));
    }
}

/// Applies `message` as [`update`] describes, leaving the folded project N falls back on to
/// [`update`].
fn dispatch(state: &mut State, message: ProjectsMessage) -> Task<Message> {
    match message {
        ProjectsMessage::OpenFolder => {
            return Task::perform(pick_folder(), |folder| {
                Message::Projects(ProjectsMessage::FolderPicked(folder))
            });
        }
        ProjectsMessage::FolderPicked(None) => {}
        ProjectsMessage::FolderPicked(Some(folder)) => {
            state.app.open_project(&folder);
        }
        ProjectsMessage::NewSession(project, scope) => {
            return new_session_sheet::open(state, project, scope);
        }
        ProjectsMessage::Close(project) => close(state, project),
        ProjectsMessage::ToggleProject(project) => toggle_project(state, project),
        ProjectsMessage::HoverProject(project) => state.projects.hovered_project = Some(project),
        ProjectsMessage::UnhoverProject(project) => {
            if state.projects.hovered_project.as_ref() == Some(&project) {
                state.projects.hovered_project = None;
            }
        }
        ProjectsMessage::ToggleChildren(session) => toggle_children(state, session),
        ProjectsMessage::StartRename(session) => {
            return start_rename(&mut state.projects, &state.app.tree(), session);
        }
        ProjectsMessage::RenameInput(typed) => {
            if let Some((_, text)) = &mut state.projects.renaming {
                *text = typed;
            }
        }
        ProjectsMessage::CommitRename => commit_rename(state),
        ProjectsMessage::CancelRename => state.projects.renaming = None,
        ProjectsMessage::ToggleArchived(project) => toggle_archived(state, project),
        ProjectsMessage::Restore(session) => restore(state, &session),
        ProjectsMessage::Archive(session) => {
            return confirm_sheet::open(state, Action::Archive, &session);
        }
        ProjectsMessage::Delete(session) => {
            return confirm_sheet::open(state, Action::Delete, &session);
        }
        ProjectsMessage::Hover(session) => state.projects.hovered = Some(session),
        ProjectsMessage::Unhover(session) => {
            if state.projects.hovered.as_ref() == Some(&session) {
                state.projects.hovered = None;
            }
        }
        ProjectsMessage::CursorDown
        | ProjectsMessage::CursorUp
        | ProjectsMessage::OpenCursor
        | ProjectsMessage::NewSessionAtCursor
        | ProjectsMessage::ArchiveAtCursor
        | ProjectsMessage::DeleteAtCursor
            if state.tab != Tab::Projects => {}
        ProjectsMessage::CursorDown => move_cursor(&mut state.projects, &state.app.tree(), true),
        ProjectsMessage::CursorUp => move_cursor(&mut state.projects, &state.app.tree(), false),
        ProjectsMessage::OpenCursor => {
            if let Some(session) = state.projects.cursor.clone() {
                pick(state, session);
            }
        }
        ProjectsMessage::NewSessionAtCursor => {
            if let Some(project) = cursor_project(&state.projects, &state.app.tree()) {
                return update(state, ProjectsMessage::NewSession(project, Scope::Project));
            }
        }
        ProjectsMessage::ArchiveAtCursor => {
            let tree = state.app.tree();
            let archivable = state.projects.cursor.clone().filter(|cursor| {
                tree.projects
                    .iter()
                    .any(|project| project.sessions.iter().any(|top| &top.id == cursor))
            });
            if let Some(session) = archivable {
                return update(state, ProjectsMessage::Archive(session));
            }
        }
        ProjectsMessage::DeleteAtCursor => {
            let tree = state.app.tree();
            let deletable = state
                .projects
                .cursor
                .clone()
                .filter(|cursor| tree.top_level(cursor).is_some());
            if let Some(session) = deletable {
                return update(state, ProjectsMessage::Delete(session));
            }
        }
    }
    Task::none()
}

/// Lets the user pick a folder in the desktop's folder dialog; `None` when it is cancelled.
async fn pick_folder() -> Option<PathBuf> {
    let folder = rfd::AsyncFileDialog::new().pick_folder().await?;
    Some(folder.path().to_path_buf())
}

/// Switches to the Session tab for the session that just became current, and moves the
/// cursor onto it. Its project opens when it is folded, and every folded session above it
/// shows its spawned sessions again (the core remembers both), so the highlighted row is in
/// sight when the user comes back to the tree.
pub(crate) fn show_current_session(state: &mut State) {
    state.tab = Tab::Session;
    state.projects.cursor = state.app.current_session();
    if let Some(current) = state.projects.cursor.clone() {
        state.projects.fallback_project = None;
        let current = &current;
        let tree = state.app.tree();
        if let Some(found) = tree.ancestors(current) {
            if found.archived {
                state
                    .projects
                    .archived_open
                    .insert(found.project.id.clone());
            }
            if found.project.fold == Fold::Folded {
                report(
                    &mut state.error,
                    state.app.toggle_project(found.project.id.clone()),
                );
            }
            for session in found.above.into_iter().filter(|session| !session.expanded) {
                report(
                    &mut state.error,
                    state.app.toggle_children(session.id.clone()),
                );
            }
        }
    }
    session_view::sync_markdown(state);
}

/// Closes `project` and forgets what the interface remembers about it and its sessions, the
/// cursor, a hover on its header and N falling back on it included.
fn close(state: &mut State, project: ProjectId) {
    let closing = state.app.tree().project_sessions(&project);
    let hovered = state.projects.hovered_project.as_ref() == Some(&project);
    let project_id = project.clone();
    if report(&mut state.error, state.app.close_project(project)).is_none() {
        return;
    }
    forget(state, &closing);
    if state
        .projects
        .cursor
        .as_ref()
        .is_some_and(|cursor| closing.contains(cursor))
    {
        state.projects.cursor = None;
    }
    if hovered {
        state.projects.hovered_project = None;
    }
    if state.projects.fallback_project.as_ref() == Some(&project_id) {
        state.projects.fallback_project = None;
    }
}

/// Forgets what the interface remembers about `sessions`, which left the tree: everything
/// the tabs keep per session ([`app::forget_sessions`]), a hovered row and a title being
/// edited.
fn forget(state: &mut State, sessions: &[SessionId]) {
    app::forget_sessions(state, sessions);
    if state
        .projects
        .renaming
        .as_ref()
        .is_some_and(|(renaming, _)| sessions.contains(renaming))
    {
        state.projects.renaming = None;
    }
    if state
        .projects
        .hovered
        .as_ref()
        .is_some_and(|hovered| sessions.contains(hovered))
    {
        state.projects.hovered = None;
    }
}

/// Hides the sessions spawned from `session`, or shows them when they are hidden; the core
/// remembers which. A cursor on a row this hides moves to `session`, so the highlighted row
/// stays in sight.
fn toggle_children(state: &mut State, session: SessionId) {
    if report(&mut state.error, state.app.toggle_children(session.clone())).is_none() {
        return;
    }
    let visible = state
        .app
        .tree()
        .visible_sessions(&state.projects.archived_open);
    if state
        .projects
        .cursor
        .as_ref()
        .is_some_and(|cursor| !visible.contains(cursor))
    {
        state.projects.cursor = Some(session);
    }
}

/// Archives `session` with the sessions below it and reports whether that worked. A cursor
/// on a row that archiving hides moves to the nearest row still shown, below it first.
pub(crate) fn archive(state: &mut State, session: SessionId) -> bool {
    let before = state
        .app
        .tree()
        .visible_sessions(&state.projects.archived_open);
    if report(&mut state.error, state.app.archive(session)).is_none() {
        return false;
    }
    hand_off_cursor(&mut state.projects, &before, &state.app.tree());
    true
}

/// Deletes `session` with the sessions below it, also removing the worktrees and branches
/// `cleanup` checks in the background, and reports whether they are gone. They
/// can be gone with an error, when closing their backend sessions or deleting their
/// transcripts failed; the error bar says so. A cursor on a deleted row moves to the nearest
/// row still shown, below it first, and the interface forgets what it remembered about the
/// deleted sessions. When the Session, Spec or Env tab showed one of them, the Projects tab is
/// shown instead.
pub(crate) fn delete(state: &mut State, session: SessionId, cleanup: &[Cleanup]) -> bool {
    let tree = state.app.tree();
    let before = tree.visible_sessions(&state.projects.archived_open);
    let family = tree.family(&session);
    let showed_one = state
        .app
        .current_session()
        .is_some_and(|current| family.contains(&current));
    let result = state.app.delete(session, cleanup);
    let after = state.app.tree();
    let gone = family
        .first()
        .is_some_and(|top| after.top_level(top).is_none());
    report(&mut state.error, result);
    if !gone {
        return false;
    }
    forget(state, &family);
    hand_off_cursor(&mut state.projects, &before, &after);
    if showed_one {
        state.tab = Tab::Projects;
    }
    true
}

/// Moves a cursor whose row `tree`, the tree as it is now, no longer shows to the nearest
/// row still shown, below it first, given the rows it showed `before`. Without such a row
/// the cursor leaves the tree.
fn hand_off_cursor(projects: &mut ProjectsState, before: &[SessionId], tree: &TreeModel) {
    let after = tree.visible_sessions(&projects.archived_open);
    let Some(position) = projects
        .cursor
        .as_ref()
        .filter(|cursor| !after.contains(cursor))
        .and_then(|cursor| before.iter().position(|row| row == cursor))
    else {
        return;
    };
    let below = before.iter().skip(position);
    let above = before.iter().take(position).rev();
    projects.cursor = below.chain(above).find(|row| after.contains(row)).cloned();
}

/// Restores the archived `session`. Its row leaves the pointer behind as it moves back to
/// the live list, so a hover on it ends, and the live row shows no strip it was not asked
/// for.
fn restore(state: &mut State, session: &SessionId) {
    if report(&mut state.error, state.app.restore(session.clone())).is_some()
        && state.projects.hovered.as_ref() == Some(session)
    {
        state.projects.hovered = None;
    }
}

/// Folds the rows of `project` away under its header, or opens them again; the core
/// remembers which. A cursor on a row this hides leaves the tree.
fn toggle_project(state: &mut State, project: ProjectId) {
    if report(&mut state.error, state.app.toggle_project(project)).is_some() {
        drop_hidden_cursor(&mut state.projects, &state.app.tree());
    }
}

/// Hides the archived sessions of `project`, or shows them when they are hidden. A cursor on
/// a row this hides leaves the tree.
fn toggle_archived(state: &mut State, project: ProjectId) {
    if !state.projects.archived_open.remove(&project) {
        state.projects.archived_open.insert(project);
    }
    drop_hidden_cursor(&mut state.projects, &state.app.tree());
}

/// Takes the cursor off `tree`, the tree as it is now, when it no longer shows the cursor's
/// row, and remembers its project for N.
fn drop_hidden_cursor(projects: &mut ProjectsState, tree: &TreeModel) {
    let visible = tree.visible_sessions(&projects.archived_open);
    if let Some(cursor) = projects.cursor.take_if(|cursor| !visible.contains(cursor)) {
        projects.fallback_project = tree.project_of(&cursor).cloned();
    }
}

/// Turns the title of `session` in `tree` into a field holding it, gives the field the focus
/// and selects its text, so typing replaces it.
fn start_rename(
    projects: &mut ProjectsState,
    tree: &TreeModel,
    session: SessionId,
) -> Task<Message> {
    let Some(title) = tree.find_node(&session).map(|node| node.title.clone()) else {
        return Task::none();
    };
    let field = rename_field_id(&session);
    projects.renaming = Some((session, title));
    Task::batch([
        iced::widget::operation::focus(field.clone()),
        iced::widget::operation::select_all(field),
    ])
}

/// The id of the field that edits the title of `session`.
fn rename_field_id(session: &SessionId) -> String {
    format!("rename-{session}")
}

/// Moves the cursor to the next session row `tree` shows (`forward`) or the previous one,
/// across projects. It stops at the first and the last row; without a cursor, forward goes
/// to the first row.
fn move_cursor(projects: &mut ProjectsState, tree: &TreeModel, forward: bool) {
    let rows = tree.visible_sessions(&projects.archived_open);
    let position = projects
        .cursor
        .as_ref()
        .and_then(|cursor| rows.iter().position(|row| row == cursor));
    let target = match (position, forward) {
        (None, true) => rows.first(),
        (None, false) => None,
        (Some(position), true) => rows.get(position.saturating_add(1)),
        (Some(position), false) => position.checked_sub(1).and_then(|above| rows.get(above)),
    };
    if let Some(target) = target {
        projects.cursor = Some(target.clone());
    }
}

/// The project of `tree` a session started from the keyboard goes to: the cursor's project;
/// without a cursor, the project whose folding took it away, or else the first project.
fn cursor_project(projects: &ProjectsState, tree: &TreeModel) -> Option<ProjectId> {
    let Some(cursor) = &projects.cursor else {
        let folded = projects
            .fallback_project
            .as_ref()
            .filter(|folded| tree.projects.iter().any(|project| &project.id == *folded));
        return folded
            .cloned()
            .or_else(|| tree.projects.first().map(|project| project.id.clone()));
    };
    tree.project_of(cursor).cloned()
}

/// The step column of a session row: a small dot in the [`step_dot`] colour and the step in
/// words, empty for a session that follows no workflow.
fn step_cell<'a>(
    step: Option<&StepCell>,
    status: Status,
    highlighted: bool,
) -> Element<'a, Message> {
    let Some(step) = step else {
        return Space::new().into();
    };
    let dot = step_dot(step, status);
    // The weaker muted colour is too faint on the selection colour.
    let label_ink: Ink = if highlighted {
        |tokens| tokens.muted_strong
    } else {
        |tokens| tokens.muted
    };
    row![
        container(Space::new())
            .width(STEP_DOT)
            .height(STEP_DOT)
            .style(move |theme: &iced::Theme| container::Style {
                background: Some(dot(theme::tokens(theme)).into()),
                border: iced::Border::default().rounded(STEP_DOT / 2.0),
                ..container::Style::default()
            }),
        text(step.label.clone())
            .size(theme::SMALL)
            .style(style::ink(label_ink))
            .wrapping(Wrapping::None),
    ]
    .spacing(6)
    .align_y(Alignment::Center)
    .into()
}

/// The colour of the dot of `step` for a session at `status`: amber while a gate waits for
/// the user, bright while the session runs and dim otherwise, as for a child whose gate its
/// orchestrator decides.
fn step_dot(step: &StepCell, status: Status) -> Ink {
    if step.gate_waiting {
        |tokens| tokens.asks_you.fg
    } else if status == Status::Running {
        |tokens| tokens.text
    } else {
        |tokens| tokens.dim
    }
}

/// One project: its header and, unless the project is folded, its session rows (their
/// activity told as of `now`), the `new session` row and the archived sessions' row with the
/// archived sessions under it while they are shown.
fn project_section<'a>(
    project: &ProjectNode,
    state: &ProjectsState,
    home: Option<&Path>,
    now: u64,
) -> Element<'a, Message> {
    let hovered = state.hovered_project.as_ref() == Some(&project.id);
    let mut section = Column::new()
        .spacing(ROW_GAP)
        .push(project_header(project, hovered, home));
    if project.fold == Fold::Folded {
        return section.into();
    }
    for session in &project.sessions {
        section = push_session(section, session, 0, state, now);
    }
    section = section.push(new_session_row(project));
    if !project.archived.is_empty() {
        let open = state.archived_open.contains(&project.id);
        section = section.push(archived_group_row(project, open));
        if open {
            for session in &project.archived {
                section = push_archived(section, session, 1, state);
            }
        }
    }
    section.into()
}

/// The project's header, found by `project-<id>`: the chevron that folds and opens the
/// project (found by `project-toggle-<id>`), its name, how many repositories it has, its
/// folder, the instruction files (`AGENTS.md`, `CLAUDE.md`) its folder holds and, while a
/// removal runs in it, `cleaning up…`. While `hovered`, the header takes the panel colour
/// and ends in the close button at the right edge, found by `close-<id>`.
fn project_header<'a>(
    project: &ProjectNode,
    hovered: bool,
    home: Option<&Path>,
) -> Element<'a, Message> {
    let folder = keep_end(&home_path::abbreviated(&project.root, home), PATH_BUDGET);
    let mut facts = row![
        text(project.name.clone()).font(theme::UI_SEMIBOLD),
        text(repository_count(project.repository_count)).style(style::muted),
        mono(folder),
    ]
    .spacing(HEADER_GAP)
    .align_y(Alignment::Center);
    for file in &project.instruction_files {
        facts = facts.push(mono(file.clone()));
    }
    if project.cleaning {
        facts = facts.push(text("cleaning up…").size(theme::SMALL).style(style::dim));
    }
    let toggle = button(
        container(icon(fold_icon(project.fold), |tokens| tokens.muted)).center(Length::Fill),
    )
    .padding(0)
    .width(SLOT_WIDTH)
    .height(Length::Fill)
    .style(style::link_button)
    .on_press(Message::Projects(ProjectsMessage::ToggleProject(
        project.id.clone(),
    )));
    // The slot reaches beyond its icon, so it starts that much further left and the facts
    // follow that much closer, as in a session row: the name sits over the sessions' titles.
    let mut line = row![
        container(toggle).id(format!("project-toggle-{}", project.id)),
        facts,
        space::horizontal(),
    ]
    .spacing(PART_GAP - SLOT_REACH)
    .height(Length::Fill)
    .align_y(Alignment::Center);
    if hovered {
        let close = button(icon(Icon::Close, |tokens| tokens.muted))
            .padding(CLOSE_PADDING)
            .width(CLOSE_SIZE)
            .height(CLOSE_SIZE)
            .style(style::row_button)
            .on_press(Message::Projects(ProjectsMessage::Close(
                project.id.clone(),
            )));
        line = line.push(container(close).id(format!("close-{}", project.id)));
    }
    let header = container(line)
        .height(HEADER_HEIGHT)
        .width(Length::Fill)
        // The close icon, not its button, ends at the text edge.
        .padding(
            Padding::ZERO
                .left(BLEED - SLOT_REACH)
                .right(BLEED - CLOSE_PADDING),
        );
    let header = if hovered {
        header.style(header_surface)
    } else {
        header
    };
    mouse_area(container(header).id(format!("project-{}", project.id)))
        .on_enter(Message::Projects(ProjectsMessage::HoverProject(
            project.id.clone(),
        )))
        .on_exit(Message::Projects(ProjectsMessage::UnhoverProject(
            project.id.clone(),
        )))
        .into()
}

/// The header of a folder being opened as a project, found by `opening-<folder>`: in a
/// project header's place, its name and its folder followed by a dim `scanning…`, with
/// nothing to fold or close yet.
fn opening_header<'a>(opening: &OpeningNode, home: Option<&Path>) -> Element<'a, Message> {
    let folder = keep_end(&home_path::abbreviated(&opening.folder, home), PATH_BUDGET);
    let facts = row![
        text(opening.name.clone()).font(theme::UI_SEMIBOLD),
        mono(folder),
        text(SCANNING).size(theme::SMALL).style(style::dim),
    ]
    .spacing(HEADER_GAP)
    .align_y(Alignment::Center);
    // The facts start where a project header's name does, after its chevron's slot.
    let line = row![space::horizontal().width(SLOT_WIDTH), facts]
        .spacing(PART_GAP - SLOT_REACH)
        .height(Length::Fill)
        .align_y(Alignment::Center);
    container(
        container(line)
            .height(HEADER_HEIGHT)
            .width(Length::Fill)
            .padding(Padding::ZERO.left(BLEED - SLOT_REACH)),
    )
    .id(format!("opening-{}", opening.folder.display()))
    .into()
}

/// What a folder being opened says until it is ready.
const SCANNING: &str = "scanning…";

/// The surface of a hovered project header: the panel colour with the rows' rounding.
fn header_surface(theme: &iced::Theme) -> container::Style {
    container::Style {
        background: Some(theme::tokens(theme).panel.into()),
        border: iced::Border::default().rounded(style::ROW_RADIUS),
        ..container::Style::default()
    }
}

/// The chevron of a project header: down while the project shows its rows, right while it is
/// folded.
fn fold_icon(fold: Fold) -> Icon {
    match fold {
        Fold::Open => Icon::ChevronDown,
        Fold::Folded => Icon::ChevronRight,
    }
}

/// How many repositories a project has, in words: `1 repo`, `3 repos`.
fn repository_count(count: usize) -> String {
    if count == 1 {
        "1 repo".to_owned()
    } else {
        format!("{count} repos")
    }
}

/// Adds the row of `session`, `depth` levels below the top-level sessions, and under it the
/// rows of the sessions spawned from it while it is expanded, else the row that sums them up.
fn push_session<'a>(
    mut section: Column<'a, Message>,
    session: &SessionNode,
    depth: u16,
    state: &ProjectsState,
    now: u64,
) -> Column<'a, Message> {
    section = section.push(session_row(session, depth, state, now));
    if session.children.is_empty() {
        return section;
    }
    if session.expanded {
        for child in &session.children {
            section = push_session(section, child, depth.saturating_add(1), state, now);
        }
    } else {
        let indent = f32::from(depth.saturating_add(1)) * LEVEL_INDENT;
        section = section.push(children_summary(session, indent));
    }
    section
}

/// The row of `session`, found by `session-<id>`, in fixed columns: the chevron that hides
/// or shows its spawned sessions (found by `toggle-<id>`), the title cell, the status pill,
/// the context usage and how long ago it was active as of `now`. A click anywhere but on the
/// chevron picks the session. The cursor's row is highlighted.
fn session_row<'a>(
    session: &SessionNode,
    depth: u16,
    state: &ProjectsState,
    now: u64,
) -> Element<'a, Message> {
    let highlighted = state.cursor.as_ref() == Some(&session.id);
    // The weaker muted colour is too faint on the selection colour.
    let (muted_ink, dim_ink): (Ink, Ink) = if highlighted {
        (|tokens| tokens.muted_strong, |tokens| tokens.muted_strong)
    } else {
        (|tokens| tokens.muted, |tokens| tokens.dim)
    };
    let shows_strip = highlighted || state.hovered.as_ref() == Some(&session.id);
    let context: Element<'a, Message> = match session.usage {
        Some((used, size)) if is_live(session.status) => text(tokens_label::usage(used, size))
            .font(theme::MONO)
            .size(theme::SMALL)
            .style(style::ink(muted_ink))
            .into(),
        _ => Space::new().into(),
    };
    let activity = text(age_label(now, session.active_at))
        .size(theme::SMALL)
        .style(style::ink(dim_ink));
    let line = row![
        title_cell(session, depth, state, highlighted, shows_strip),
        container(badge::session(
            session.status,
            session.background,
            session.delegated
        ))
        .width(STATUS_COLUMN),
        container(step_cell(
            session.step.as_ref(),
            session.status,
            highlighted
        ))
        .width(STEP_COLUMN),
        container(context).align_right(CONTEXT_COLUMN),
        container(activity).align_right(ACTIVITY_COLUMN),
    ]
    .spacing(COLUMN_GAP)
    .align_y(Alignment::Center);

    let row = tree_row(
        format!("session-{}", session.id),
        f32::from(depth) * LEVEL_INDENT,
        chevron(session, muted_ink),
        line.into(),
        highlighted,
        Message::SelectSession(session.id.clone()),
    );
    mouse_area(row)
        .on_enter(Message::Projects(ProjectsMessage::Hover(
            session.id.clone(),
        )))
        .on_exit(Message::Projects(ProjectsMessage::Unhover(
            session.id.clone(),
        )))
        .into()
}

/// Whether a session in `status` has a context worth showing: it runs, waits for the user or
/// waits for its next prompt.
fn is_live(status: Status) -> bool {
    matches!(
        status,
        Status::Running | Status::WaitingForYou | Status::Idle
    )
}

/// The title cell of a session's row: the skill chip, the title (or the field that edits
/// it), the repository, the orchestrator pill and the pill for a child that waits for the
/// user, clipped to the space the cell has left, and, while `shows_strip`, the action strip
/// at its right end, which is never clipped. The title's tooltip is the full first prompt.
fn title_cell<'a>(
    session: &SessionNode,
    depth: u16,
    state: &ProjectsState,
    highlighted: bool,
    shows_strip: bool,
) -> Element<'a, Message> {
    let (main_ink, muted_ink): (Ink, Ink) = if highlighted {
        (|tokens| tokens.text_strong, |tokens| tokens.muted_strong)
    } else {
        (|tokens| tokens.text_soft, |tokens| tokens.muted)
    };
    // A spawned session is one of several that share their parent's task, so what tells
    // them apart, the repository, leads; else the title leads.
    let leads_with_repository = depth > 0 && !session.scope_label.is_empty();
    let title_ink = if leads_with_repository {
        muted_ink
    } else {
        main_ink
    };
    let title = state
        .renaming
        .as_ref()
        .filter(|(renamed, _)| renamed == &session.id)
        .map_or_else(
            || title_text(session, highlighted && !leads_with_repository, title_ink),
            |(_, typed)| rename_field(session, typed),
        );
    let repository: Option<Element<'a, Message>> = (!session.scope_label.is_empty()).then(|| {
        let ink = if leads_with_repository {
            main_ink
        } else {
            muted_ink
        };
        text(session.scope_label.clone())
            .style(style::ink(ink))
            .wrapping(Wrapping::None)
            .into()
    });
    let (first, second) = if leads_with_repository {
        (repository, Some(title))
    } else {
        (Some(title), repository)
    };
    // The skill chip sits closer to what follows it than the parts of the row do.
    let mut lead = Row::new().spacing(SKILL_GAP).align_y(Alignment::Center);
    if let Some(skill) = &session.skill {
        lead = lead.push(
            text(skill.clone())
                .font(theme::MONO)
                .size(theme::SMALL)
                .style(style::ink(muted_ink))
                .wrapping(Wrapping::None),
        );
    }
    let mut named = Row::new().spacing(PART_GAP).align_y(Alignment::Center);
    named = named.push(lead.extend(first));
    named = named.extend(second);
    if session.orchestrator {
        named = named.push(badge::orchestrator());
    }
    if session.attention
        && !matches!(
            session.status,
            Status::WaitingForYou | Status::Interrupted | Status::Failed
        )
    {
        named = named.push(badge::pill(CHILD_ASKS_YOU, |tokens| tokens.asks_you));
    }
    let mut cell = Row::new()
        .spacing(PART_GAP)
        .align_y(Alignment::Center)
        .push(container(named).width(Length::Fill).clip(true));
    if shows_strip {
        cell = cell.push(action_strip(session, depth));
    }
    cell.into()
}

/// The title of `session`, cut to [`TITLE_BUDGET`] characters and never wrapped, with the
/// first prompt as its tooltip. It is drawn in `ink`, in the medium weight when `strong`.
fn title_text<'a>(session: &SessionNode, strong: bool, ink: Ink) -> Element<'a, Message> {
    let font = if strong { theme::UI_MEDIUM } else { theme::UI };
    let label = text(keep_start(&session.title, TITLE_BUDGET))
        .font(font)
        .style(style::ink(ink))
        .wrapping(Wrapping::None);
    if session.first_prompt.is_empty() {
        label.into()
    } else {
        tooltip(
            label,
            container(
                text(keep_head(
                    &session.first_prompt,
                    PROMPT_TIP_LINES,
                    PROMPT_TIP_BUDGET,
                ))
                .size(theme::SMALL),
            )
            .max_width(PROMPT_TIP_WIDTH)
            .padding([4, 8])
            .style(style::card),
            tooltip::Position::Bottom,
        )
        .into()
    }
}

/// The field that edits the title of `session`, holding `typed`, found by `rename-<id>`.
/// Enter commits what it holds.
fn rename_field<'a>(session: &SessionNode, typed: &str) -> Element<'a, Message> {
    text_input("title", typed)
        .id(rename_field_id(&session.id))
        .on_input(|typed| Message::Projects(ProjectsMessage::RenameInput(typed)))
        .on_submit(Message::Projects(ProjectsMessage::CommitRename))
        .padding([1, 6])
        .width(Length::Fill)
        .into()
}

/// The row under a folded orchestrator, found by `summary-<id>`, that sums up its children,
/// its text where the title of a child row whose slot sits `indent` right of the project
/// headers would start.
fn children_summary<'a>(session: &SessionNode, indent: f32) -> Element<'a, Message> {
    container(
        text(children_summary_text(session))
            .size(theme::SMALL)
            .style(style::muted)
            .wrapping(Wrapping::None),
    )
    .id(format!("summary-{}", session.id))
    .height(SUMMARY_HEIGHT)
    .center_y(SUMMARY_HEIGHT)
    .padding(Padding::ZERO.left(BLEED + indent + SESSION_INDENT))
    .into()
}

/// What the summary row of `session` says: how many children it has, then each child's
/// repository and status word, joined by dots: `2 children · api running · web idle`.
fn children_summary_text(session: &SessionNode) -> String {
    let count = session.children.len();
    let mut parts = vec![if count == 1 {
        "1 child".to_owned()
    } else {
        format!("{count} children")
    }];
    parts.extend(session.children.iter().map(|child| {
        format!(
            "{} {}",
            child.scope_label,
            badge::session_word(child.status, child.background, child.delegated)
        )
        .trim()
        .to_owned()
    }));
    parts.join(" · ")
}

/// The actions at the right end of a live session's row: the archive button, found by
/// `archive-<id>`, and the delete button, found by `delete-<id>`, each with its key. While
/// the session (or one below it) runs or waits they read `Stop and archive` and `Stop and
/// delete`. A session `depth` levels down goes with its orchestrator, so its buttons cannot
/// be pressed and say so in a tooltip.
fn action_strip<'a>(session: &SessionNode, depth: u16) -> Element<'a, Message> {
    let stops = matches!(session.status, Status::Running | Status::WaitingForYou);
    let (archive_label, delete_label) = if stops {
        ("Stop and archive", "Stop and delete")
    } else {
        ("Archive", "Delete")
    };
    let archive = strip_button(
        row![
            text(archive_label).size(theme::SMALL),
            badge::key_cap_on_archive("A")
        ],
        style::archive_button,
    );
    let delete = strip_button(
        row![
            text(delete_label).size(theme::SMALL),
            badge::key_cap_on_delete("D")
        ],
        style::delete_button,
    );
    let (archive, delete): (Element<'a, Message>, Element<'a, Message>) = if depth == 0 {
        (
            archive
                .on_press(Message::Projects(ProjectsMessage::Archive(
                    session.id.clone(),
                )))
                .into(),
            delete
                .on_press(Message::Projects(ProjectsMessage::Delete(
                    session.id.clone(),
                )))
                .into(),
        )
    } else {
        (
            with_tooltip(archive, ARCHIVE_WITH_ORCHESTRATOR),
            with_tooltip(delete, DELETE_WITH_ORCHESTRATOR),
        )
    };
    row![
        container(archive).id(format!("archive-{}", session.id)),
        container(delete).id(format!("delete-{}", session.id)),
    ]
    .spacing(6)
    .align_y(Alignment::Center)
    .into()
}

/// A button of the action strip around `content`, drawn in `style`.
fn strip_button(
    content: Row<'_, Message>,
    style: fn(&iced::Theme, button::Status) -> button::Style,
) -> button::Button<'_, Message> {
    button(content.spacing(6).align_y(Alignment::Center))
        .padding([2, 8])
        .style(style)
}

/// `control` with `tip` above it while the pointer is on it.
fn with_tooltip<'a>(control: button::Button<'a, Message>, tip: &'a str) -> Element<'a, Message> {
    tooltip(
        control,
        container(text(tip).size(theme::SMALL))
            .padding([2, 8])
            .style(style::card),
        tooltip::Position::Top,
    )
    .into()
}

/// The slot at the left of a session's row: empty for a session nothing was spawned from,
/// else the chevron that hides or shows the spawned sessions, pointing down while they are
/// shown and right while they are hidden.
fn chevron<'a>(session: &SessionNode, ink: Ink) -> Element<'a, Message> {
    if session.children.is_empty() {
        return Space::new().width(SLOT_WIDTH).into();
    }
    let direction = if session.expanded {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    };
    let toggle = button(container(icon(direction, ink)).center(Length::Fill))
        .padding(0)
        .width(SLOT_WIDTH)
        .height(Length::Fill)
        .style(style::link_button)
        .on_press(Message::Projects(ProjectsMessage::ToggleChildren(
            session.id.clone(),
        )));
    container(toggle)
        .id(format!("toggle-{}", session.id))
        .into()
}

/// The row that shows or hides the archived sessions of `project`, found by
/// `archived-<id>`: a chevron pointing down while they are shown, the archive icon and how
/// many top-level sessions are archived.
fn archived_group_row<'a>(project: &ProjectNode, open: bool) -> Element<'a, Message> {
    let direction = if open {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    };
    let slot = container(icon(direction, |tokens| tokens.dim)).center_x(SLOT_WIDTH);
    let line = parts(row![
        icon(Icon::Archive, |tokens| tokens.dim),
        text(format!("archived · {}", project.archived.len())).style(style::muted),
    ]);
    container(tree_row(
        format!("archived-{}", project.id),
        SESSION_INDENT,
        slot.into(),
        line,
        false,
        Message::Projects(ProjectsMessage::ToggleArchived(project.id.clone())),
    ))
    .padding(Padding::ZERO.top(ARCHIVED_GAP))
    .into()
}

/// Adds the row of the archived `session`, `depth` levels below the top-level sessions, and
/// under it the rows of the sessions archived with it while it is expanded, else the row
/// that sums them up.
fn push_archived<'a>(
    mut section: Column<'a, Message>,
    session: &SessionNode,
    depth: u16,
    state: &ProjectsState,
) -> Column<'a, Message> {
    section = section.push(archived_row(session, depth, state));
    if session.children.is_empty() {
        return section;
    }
    let below = depth.saturating_add(1);
    if session.expanded {
        for child in &session.children {
            section = push_archived(section, child, below, state);
        }
    } else {
        let indent = SESSION_INDENT + f32::from(below) * LEVEL_INDENT;
        section = section.push(children_summary(session, indent));
    }
    section
}

/// The row of an archived session, found by `session-<id>`: its title, when it was archived
/// and, while the pointer is over the row of a session archived on its own account, the
/// archived action strip at the right edge. A click anywhere else opens the session
/// read-only.
fn archived_row<'a>(
    session: &SessionNode,
    depth: u16,
    state: &ProjectsState,
) -> Element<'a, Message> {
    let highlighted = state.cursor.as_ref() == Some(&session.id);
    let (title_ink, meta_ink): (Ink, Ink) = if highlighted {
        (|tokens| tokens.text_strong, |tokens| tokens.muted_strong)
    } else {
        (|tokens| tokens.muted, |tokens| tokens.muted)
    };
    let mut line = Row::new()
        .push(text(keep_start(&session.title, TITLE_BUDGET)).style(style::ink(title_ink)));
    if let Some(mark) = session.archived {
        line = line.push(
            text(format!("archived {}", day_month(mark.at)))
                .size(theme::SMALL)
                .style(style::ink(meta_ink)),
        );
    }
    line = line.push(space::horizontal());
    if depth == 1 && state.hovered.as_ref() == Some(&session.id) {
        line = line.push(archived_strip(session));
    }
    let row = tree_row(
        format!("session-{}", session.id),
        SESSION_INDENT + f32::from(depth) * LEVEL_INDENT,
        chevron(session, meta_ink),
        parts(line),
        highlighted,
        Message::SelectSession(session.id.clone()),
    );
    mouse_area(row)
        .on_enter(Message::Projects(ProjectsMessage::Hover(
            session.id.clone(),
        )))
        .on_exit(Message::Projects(ProjectsMessage::Unhover(
            session.id.clone(),
        )))
        .into()
}

/// The actions at the right end of an archived session's row, drawn like a live row's: the
/// restore button, found by `restore-<id>`, and the delete button with its key, found by
/// `delete-<id>`.
fn archived_strip<'a>(session: &SessionNode) -> Element<'a, Message> {
    let restore = strip_button(
        row![text("Restore").size(theme::SMALL)],
        style::archive_button,
    )
    .on_press(Message::Projects(ProjectsMessage::Restore(
        session.id.clone(),
    )));
    let delete = strip_button(
        row![
            text("Delete").size(theme::SMALL),
            badge::key_cap_on_delete("D")
        ],
        style::delete_button,
    )
    .on_press(Message::Projects(ProjectsMessage::Delete(
        session.id.clone(),
    )));
    row![
        container(restore).id(format!("restore-{}", session.id)),
        container(delete).id(format!("delete-{}", session.id)),
    ]
    .spacing(6)
    .align_y(Alignment::Center)
    .into()
}

/// The row with a plus and `new session` that opens the new-session sheet for `project`,
/// found by `new-session-<id>`. The sheet picks the repository, so a project whose sheet
/// offers several adds a hint that says so.
fn new_session_row<'a>(project: &ProjectNode) -> Element<'a, Message> {
    let mut label = row![text("new session").style(style::muted)];
    if project.repositories.len() >= 2 {
        label = label.push(text(NEW_SESSION_HINT).size(theme::SMALL).style(style::dim));
    }
    tree_row(
        format!("new-session-{}", project.id),
        SESSION_INDENT,
        plus(),
        parts(label),
        false,
        Message::Projects(ProjectsMessage::NewSession(
            project.id.clone(),
            Scope::Project,
        )),
    )
}

/// The tree's last row, level with the project headers, which opens the folder dialog.
fn open_folder_row<'a>() -> Element<'a, Message> {
    tree_row(
        "open-project-folder".to_owned(),
        0.0,
        plus(),
        parts(row![text("open project folder").style(style::muted)]),
        false,
        Message::Projects(ProjectsMessage::OpenFolder),
    )
}

/// One clickable row of the tree, found by `id`: `slot` then `line`, with the slot's icon
/// `indent` right of the project headers. A click that `slot` does not take sends
/// `on_press`. The row's background reaches [`BLEED`] beyond the text on both sides: the
/// selection colour when `highlighted`, else a faint one while hovered.
fn tree_row<'a>(
    id: String,
    indent: f32,
    slot: Element<'a, Message>,
    line: Element<'a, Message>,
    highlighted: bool,
    on_press: Message,
) -> Element<'a, Message> {
    // The slot reaches beyond its icon, so it starts that much further left and the line
    // follows that much closer.
    let content = row![slot, line]
        .spacing(PART_GAP - SLOT_REACH)
        .height(Length::Fill)
        .align_y(Alignment::Center);
    let style = if highlighted {
        style::selected_row_button
    } else {
        style::row_button
    };
    button(container(content).id(id))
        .padding(Padding::ZERO.left(BLEED + indent - SLOT_REACH).right(BLEED))
        .width(Length::Fill)
        .height(ROW_HEIGHT)
        .style(style)
        .on_press(on_press)
        .into()
}

/// The parts of a row's line, spaced apart and centred on one another.
fn parts(line: Row<'_, Message>) -> Element<'_, Message> {
    line.spacing(PART_GAP).align_y(Alignment::Center).into()
}

/// The plus in the slot of a row that adds something.
fn plus<'a>() -> Element<'a, Message> {
    container(icon(Icon::Plus, |tokens| tokens.dim))
        .center_x(SLOT_WIDTH)
        .into()
}

/// A path or file name: monospaced, small and dim.
fn mono<'a>(label: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    text(label)
        .font(theme::MONO)
        .size(theme::SMALL)
        .style(style::dim)
        .into()
}

/// The line at the bottom of the tab: on the left, each with its dot, how many live sessions
/// of `tree` run, wait for the user, were interrupted and failed, then how many are idle and
/// done joined by dots, then, dim, how many are archived, every count only when there is one; on the right
/// the keys.
fn status_line<'a>(tree: &TreeModel) -> Element<'a, Message> {
    let count = |status: Status| tree.sessions_showing(status);
    let dotted: [(Status, &str, Ink); 4] = [
        (Status::Running, "running", |tokens| tokens.running.fg),
        (Status::WaitingForYou, "asks you", |tokens| {
            tokens.asks_you.fg
        }),
        (Status::Interrupted, "interrupted", |tokens| {
            tokens.asks_you.fg
        }),
        (Status::Failed, "failed", |tokens| tokens.failed.fg),
    ];
    let gates = tree.gates_waiting();
    let mut line = Row::new().spacing(18).align_y(Alignment::Center);
    for (status, label, dot) in dotted {
        let mut count = count(status);
        if status == Status::WaitingForYou {
            // A gate is told on its own, so `asks you` counts only the rest.
            count -= gates;
            if gates > 0 {
                let noun = if gates == 1 { "gate" } else { "gates" };
                line = line.push(
                    row![badge::dot(dot), small(format!("{gates} {noun} waiting"))]
                        .spacing(6)
                        .align_y(Alignment::Center),
                );
            }
        }
        if count > 0 {
            line = line.push(
                row![badge::dot(dot), small(format!("{count} {label}"))]
                    .spacing(6)
                    .align_y(Alignment::Center),
            );
        }
    }
    let settled: Vec<Element<'a, Message>> = [(Status::Idle, "idle"), (Status::Done, "done")]
        .into_iter()
        .filter_map(|(status, label)| {
            let count = count(status);
            (count > 0).then(|| small(format!("{count} {label}")))
        })
        .collect();
    if !settled.is_empty() {
        let mut parts = Row::new().spacing(6).align_y(Alignment::Center);
        for (index, part) in settled.into_iter().enumerate() {
            if index > 0 {
                parts = parts.push(small("·"));
            }
            parts = parts.push(part);
        }
        line = line.push(parts);
    }
    let archived: usize = tree
        .projects
        .iter()
        .map(|project| project.archived.len())
        .sum();
    if archived > 0 {
        line = line.push(
            text(format!("{archived} archived"))
                .size(theme::SMALL)
                .style(style::dim),
        );
    }
    let hints = KEY_HINTS.into_iter().map(|(keys, action)| {
        row![badge::key_cap(keys), small(action)]
            .spacing(6)
            .align_y(Alignment::Center)
            .into()
    });
    line = line
        .push(space::horizontal())
        .push(Row::with_children(hints).spacing(14));
    column![
        style::hairline(|tokens| tokens.border),
        container(line)
            .center_y(STATUS_HEIGHT - 1.0)
            .padding([0.0, TREE_MARGIN_X])
            .style(style::fill(|tokens| tokens.panel)),
    ]
    .into()
}

/// Small muted text, as the status line writes it.
fn small<'a>(label: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    text(label).size(theme::SMALL).style(style::muted).into()
}

#[cfg(test)]
mod tests;
