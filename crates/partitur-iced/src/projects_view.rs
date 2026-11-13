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
//! the user, clipped when they outgrow the cell; the title's tooltip is the first prompt.
//! A spawned session sits one level deeper than its parent and leads with its repository.
//! An orchestrator starts folded: under it a row sums up its children,
//! `2 children · api running · web idle`. A click on a title moves the cursor onto its row;
//! a double click, or F2 on the cursor's row, turns the title into a field, where Enter
//! keeps the typed title and Escape the old one; highlighting or picking another row,
//! changing the tab or hiding the row closes the field too. One row is highlighted: the
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
//! button. The tree's last row opens another project folder.
//!
//! Deleting a session moves the cursor to the nearest row still shown and forgets what the
//! interface remembered about the deleted sessions; when the Session or Env tab showed one
//! of them, the Projects tab is shown instead. Folding a project takes the cursor off its
//! rows; N then starts a session in that project until a row has the cursor again. Picking
//! a session elsewhere (the jump panel, a card) opens its project when it is folded.
//!
//! The status line counts the live sessions that run and that wait for the user, each with
//! its dot, then those that are idle, done and failed, joined by dots, and, dim, the archived
//! ones, leaving out every count of none. On the right it names the keys `Enter`, `G`, `N` and `A`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use iced::widget::text::Wrapping;
use iced::widget::{
    Column, Row, Space, button, column, container, mouse_area, row, scrollable, space, text,
    text_input, tooltip,
};
use iced::{Alignment, Color, Element, Length, Padding, Task};
use partitur_core::{
    Cleanup, Fold, ProjectId, ProjectNode, Scope, SessionId, SessionNode, Status, StepCell,
    TreeModel, age_label, day_month,
};

use crate::app::{Message, State, Tab, report};
use crate::confirm_sheet::{self, Action};
use crate::ellipsis::{keep_end, keep_start};
use crate::icon::{self, Icon, icon};
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

/// The width of a session row's status column, which holds its pill on the left.
const STATUS_COLUMN: f32 = 88.0;

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

/// The name of the instruction file every agent reads.
const AGENTS_MD: &str = "AGENTS.md";

/// The name of the instruction file Claude Code reads.
const CLAUDE_MD: &str = "CLAUDE.md";

/// Why a child session's archive button cannot be pressed.
const ARCHIVE_WITH_ORCHESTRATOR: &str = "archive the orchestrator";

/// Why a child session's delete button cannot be pressed.
const DELETE_WITH_ORCHESTRATOR: &str = "delete the orchestrator";

/// The pill that marks a session with a session below it that waits for the user.
pub(crate) const CHILD_ASKS_YOU: &str = "child asks you";

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
    /// The user picked a session.
    Select(SessionId),
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
    /// The user clicked a session's title once: its row takes the cursor.
    Highlight(SessionId),
    /// The user double-clicked a session's title or pressed F2 on its row: the title turns
    /// into a field that holds it.
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
    if tree.projects.is_empty() {
        content = content
            .push(container(text("No projects open").style(style::muted)).padding([0.0, BLEED]));
    }
    for project in &tree.projects {
        content = content.push(project_section(project, state, home.as_deref(), now));
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
    if state.projects.cursor.is_some() {
        state.projects.fallback_project = None;
    }
    if let Some((renamed, _)) = &state.projects.renaming
        && !visible_sessions(&state.app.tree(), &state.projects).contains(renamed)
    {
        state.projects.renaming = None;
    }
    task
}

/// Closes the title field and hands what it holds to the core as the session's title.
fn commit_rename(state: &mut State) {
    if let Some((session, title)) = state.projects.renaming.take() {
        report(&mut state.error, state.app.rename_session(session, &title));
    }
}

/// Closes the title field unless it edits `session`: the user went on to another row.
fn end_rename_unless(state: &mut State, session: &SessionId) {
    if state
        .projects
        .renaming
        .as_ref()
        .is_some_and(|(renamed, _)| renamed != session)
    {
        state.projects.renaming = None;
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
            report(&mut state.error, state.app.open_project(&folder));
        }
        ProjectsMessage::NewSession(project, scope) => {
            return new_session_sheet::open(state, project, scope);
        }
        ProjectsMessage::Select(session) => {
            end_rename_unless(state, &session);
            if report(&mut state.error, state.app.select_session(session)).is_some() {
                show_current_session(state);
            }
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
        ProjectsMessage::Highlight(session) => {
            end_rename_unless(state, &session);
            state.projects.cursor = Some(session);
        }
        ProjectsMessage::StartRename(session) => return start_rename(state, session),
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
        ProjectsMessage::CursorDown => move_cursor(state, true),
        ProjectsMessage::CursorUp => move_cursor(state, false),
        ProjectsMessage::OpenCursor => {
            if let Some(session) = state.projects.cursor.clone() {
                return update(state, ProjectsMessage::Select(session));
            }
        }
        ProjectsMessage::NewSessionAtCursor => {
            if let Some(project) = cursor_project(state) {
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
                .filter(|cursor| top_level(&tree, cursor).is_some());
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
        let mut above = Vec::new();
        let mut folded = None;
        for project in &tree.projects {
            let live = sessions_above(&project.sessions, current, &mut above);
            let archived = !live && sessions_above(&project.archived, current, &mut above);
            if archived {
                state.projects.archived_open.insert(project.id.clone());
            }
            if live || archived {
                folded = (project.fold == Fold::Folded).then(|| project.id.clone());
                break;
            }
        }
        if let Some(project) = folded {
            report(&mut state.error, state.app.toggle_project(project));
        }
        for session in above.into_iter().filter(|session| !session.expanded) {
            report(
                &mut state.error,
                state.app.toggle_children(session.id.clone()),
            );
        }
    }
    session_view::sync_markdown(state);
}

/// Looks for `target` among `sessions` and the sessions below them. When it is found,
/// `above` holds the sessions it was spawned from, outermost first, and the result is
/// `true`; else `above` is left as it was.
fn sessions_above<'a>(
    sessions: &'a [SessionNode],
    target: &SessionId,
    above: &mut Vec<&'a SessionNode>,
) -> bool {
    for session in sessions {
        if &session.id == target {
            return true;
        }
        above.push(session);
        if sessions_above(&session.children, target, above) {
            return true;
        }
        above.pop();
    }
    false
}

/// Closes `project` and forgets what the interface remembers about it and its sessions, the
/// cursor, a hover on its header and N falling back on it included.
fn close(state: &mut State, project: ProjectId) {
    let closing = project_sessions(&state.app.tree(), &project);
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

/// Forgets what the interface remembers about `sessions`, which left the tree: parsed
/// answers, measured row heights, expanded entries, approval card drafts, a hovered row and a
/// title being edited.
fn forget(state: &mut State, sessions: &[SessionId]) {
    state
        .session
        .expanded
        .retain(|(session, _)| !sessions.contains(session));
    state
        .session
        .expanded_tools
        .retain(|(session, _)| !sessions.contains(session));
    state
        .session
        .proposals
        .retain(|(session, _), _| !sessions.contains(session));
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
    for session in sessions {
        state.session.markdown.forget(session);
        state.session.heights.forget(session);
    }
}

/// Hides the sessions spawned from `session`, or shows them when they are hidden; the core
/// remembers which. A cursor on a row this hides moves to `session`, so the highlighted row
/// stays in sight.
fn toggle_children(state: &mut State, session: SessionId) {
    if report(&mut state.error, state.app.toggle_children(session.clone())).is_none() {
        return;
    }
    let visible = visible_sessions(&state.app.tree(), &state.projects);
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
    let before = visible_sessions(&state.app.tree(), &state.projects);
    if report(&mut state.error, state.app.archive(session)).is_none() {
        return false;
    }
    hand_off_cursor(state, &before);
    true
}

/// Deletes `session` with the sessions below it, also removing the worktrees and branches
/// `cleanup` checks in the background, and reports whether they are gone. They
/// can be gone with an error, when closing their backend sessions or deleting their
/// transcripts failed; the error bar says so. A cursor on a deleted row moves to the nearest
/// row still shown, below it first, and the interface forgets what it remembered about the
/// deleted sessions. When the Session or Env tab showed one of them, the Projects tab is
/// shown instead.
pub(crate) fn delete(state: &mut State, session: SessionId, cleanup: &[Cleanup]) -> bool {
    let tree = state.app.tree();
    let before = visible_sessions(&tree, &state.projects);
    let mut family = Vec::new();
    if let Some(node) = top_level(&tree, &session) {
        collect_sessions(std::slice::from_ref(node), Shown::All, &mut family);
    }
    let showed_one = state
        .app
        .current_session()
        .is_some_and(|current| family.contains(&current));
    let result = state.app.delete(session, cleanup);
    let gone = family
        .first()
        .is_some_and(|top| top_level(&state.app.tree(), top).is_none());
    report(&mut state.error, result);
    if !gone {
        return false;
    }
    forget(state, &family);
    hand_off_cursor(state, &before);
    if showed_one {
        state.tab = Tab::Projects;
    }
    true
}

/// Moves a cursor whose row the tree no longer shows to the nearest row still shown, below
/// it first, given the rows it showed `before`. Without such a row the cursor leaves the
/// tree.
fn hand_off_cursor(state: &mut State, before: &[SessionId]) {
    let after = visible_sessions(&state.app.tree(), &state.projects);
    let Some(position) = state
        .projects
        .cursor
        .as_ref()
        .filter(|cursor| !after.contains(cursor))
        .and_then(|cursor| before.iter().position(|row| row == cursor))
    else {
        return;
    };
    let below = before.iter().skip(position);
    let above = before.iter().take(position).rev();
    state.projects.cursor = below.chain(above).find(|row| after.contains(row)).cloned();
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
        drop_hidden_cursor(state);
    }
}

/// Hides the archived sessions of `project`, or shows them when they are hidden. A cursor on
/// a row this hides leaves the tree.
fn toggle_archived(state: &mut State, project: ProjectId) {
    if !state.projects.archived_open.remove(&project) {
        state.projects.archived_open.insert(project);
    }
    drop_hidden_cursor(state);
}

/// Takes the cursor off the tree when the tree no longer shows its row, and remembers its
/// project for N.
fn drop_hidden_cursor(state: &mut State) {
    let tree = state.app.tree();
    let visible = visible_sessions(&tree, &state.projects);
    if let Some(cursor) = state
        .projects
        .cursor
        .take_if(|cursor| !visible.contains(cursor))
    {
        state.projects.fallback_project = project_of(&tree, &cursor);
    }
}

/// The project in `tree` that holds `session`, live or archived, at any depth.
fn project_of(tree: &TreeModel, session: &SessionId) -> Option<ProjectId> {
    tree.projects
        .iter()
        .find(|project| project_sessions(tree, &project.id).contains(session))
        .map(|project| project.id.clone())
}

/// Turns the title of `session` into a field holding it, gives the field the focus and
/// selects its text, so typing replaces it.
fn start_rename(state: &mut State, session: SessionId) -> Task<Message> {
    let tree = state.app.tree();
    let Some(title) = find_node(&tree, &session).map(|node| node.title.clone()) else {
        return Task::none();
    };
    let field = rename_field_id(&session);
    state.projects.renaming = Some((session, title));
    Task::batch([
        iced::widget::operation::focus(field.clone()),
        iced::widget::operation::select_all(field),
    ])
}

/// The id of the field that edits the title of `session`.
fn rename_field_id(session: &SessionId) -> String {
    format!("rename-{session}")
}

/// The live or archived session `session` of `tree`, at any depth.
fn find_node<'a>(tree: &'a TreeModel, session: &SessionId) -> Option<&'a SessionNode> {
    fn among<'a>(sessions: &'a [SessionNode], session: &SessionId) -> Option<&'a SessionNode> {
        sessions.iter().find_map(|node| {
            if &node.id == session {
                Some(node)
            } else {
                among(&node.children, session)
            }
        })
    }
    tree.projects.iter().find_map(|project| {
        among(&project.sessions, session).or_else(|| among(&project.archived, session))
    })
}

/// Moves the cursor to the next visible session row (`forward`) or the previous one, across
/// projects. It stops at the first and the last row; without a cursor, forward goes to the
/// first row.
fn move_cursor(state: &mut State, forward: bool) {
    let rows = visible_sessions(&state.app.tree(), &state.projects);
    let position = state
        .projects
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
        state.projects.cursor = Some(target.clone());
    }
}

/// The project a session started from the keyboard goes to: the cursor's project; without a
/// cursor, the project whose folding took it away, or else the first project.
fn cursor_project(state: &State) -> Option<ProjectId> {
    let tree = state.app.tree();
    let Some(cursor) = &state.projects.cursor else {
        let folded = state
            .projects
            .fallback_project
            .as_ref()
            .filter(|folded| tree.projects.iter().any(|project| &project.id == *folded));
        return folded
            .cloned()
            .or_else(|| tree.projects.first().map(|project| project.id.clone()));
    };
    project_of(&tree, cursor)
}

/// The top-level session `session` of `tree`, live or archived, or `None` when it is a
/// child or gone.
fn top_level<'a>(tree: &'a TreeModel, session: &SessionId) -> Option<&'a SessionNode> {
    tree.projects
        .iter()
        .flat_map(|project| project.sessions.iter().chain(&project.archived))
        .find(|top| &top.id == session)
}

/// The sessions of `project` in `tree`, spawned and archived ones included.
fn project_sessions(tree: &TreeModel, project: &ProjectId) -> Vec<SessionId> {
    let mut ids = Vec::new();
    if let Some(project) = tree.projects.iter().find(|open| &open.id == project) {
        collect_sessions(&project.sessions, Shown::All, &mut ids);
        collect_sessions(&project.archived, Shown::All, &mut ids);
    }
    ids
}

/// The sessions whose rows the tree shows, top to bottom: of every project of `tree` that is
/// not folded, its live sessions and, while `state` shows them, its archived ones, each
/// unless a folded session above it hides it.
fn visible_sessions(tree: &TreeModel, state: &ProjectsState) -> Vec<SessionId> {
    let mut ids = Vec::new();
    for project in &tree.projects {
        if project.fold == Fold::Folded {
            continue;
        }
        collect_sessions(&project.sessions, Shown::Expanded, &mut ids);
        if state.archived_open.contains(&project.id) {
            collect_sessions(&project.archived, Shown::Expanded, &mut ids);
        }
    }
    ids
}

/// Which children [`collect_sessions`] takes: those of expanded sessions only, or all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    /// The children the tree shows: those of sessions that are expanded.
    Expanded,
    /// Every child, shown or not.
    All,
}

/// Appends `sessions` to `ids` in the tree's order, each followed by the sessions spawned
/// from it when `shown` takes them.
fn collect_sessions(sessions: &[SessionNode], shown: Shown, ids: &mut Vec<SessionId>) {
    for session in sessions {
        ids.push(session.id.clone());
        if shown == Shown::All || session.expanded {
            collect_sessions(&session.children, shown, ids);
        }
    }
}

/// How many of `sessions` and the sessions below them show `status`.
fn sessions_showing(sessions: &[SessionNode], status: Status) -> usize {
    sessions
        .iter()
        .map(|session| {
            usize::from(session.status == status) + sessions_showing(&session.children, status)
        })
        .sum()
}

/// How many of `sessions` and the sessions below them wait for the user at a gate.
fn gates_waiting(sessions: &[SessionNode]) -> usize {
    sessions
        .iter()
        .map(|session| {
            usize::from(
                session.status == Status::WaitingForYou
                    && session.step.as_ref().is_some_and(|step| step.gate_waiting),
            ) + gates_waiting(&session.children)
        })
        .sum()
}

/// The step column of a session row: a small dot and the step in words, empty for a session
/// that follows no workflow. The dot is amber while a gate waits for the user, bright while
/// the session runs and dim otherwise.
fn step_cell<'a>(
    step: Option<&StepCell>,
    status: Status,
    highlighted: bool,
) -> Element<'a, Message> {
    let Some(step) = step else {
        return Space::new().into();
    };
    let dot: Ink = if step.gate_waiting {
        |tokens| tokens.asks_you.fg
    } else if status == Status::Running {
        |tokens| tokens.text
    } else {
        |tokens| tokens.dim
    };
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
    for file in instruction_files(
        project.has_global_instructions,
        project.has_global_claude_md,
    ) {
        facts = facts.push(mono(file));
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

/// The names of the instruction files a folder holds, given whether it has an `AGENTS.md`
/// and whether it has a `CLAUDE.md`.
fn instruction_files(agents_md: bool, claude_md: bool) -> impl Iterator<Item = &'static str> {
    [(agents_md, AGENTS_MD), (claude_md, CLAUDE_MD)]
        .into_iter()
        .filter_map(|(present, name)| present.then_some(name))
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
/// the context usage and how long ago it was active as of `now`. A click on the title moves
/// the cursor onto the row and a double click edits the title; a click anywhere else but on
/// the chevron picks the session. The cursor's row is highlighted.
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
        container(badge::status(session.status)).width(STATUS_COLUMN),
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
        ProjectsMessage::Select(session.id.clone()),
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
    if session.attention && session.status != Status::WaitingForYou {
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

/// The title of `session`, cut to [`TITLE_BUDGET`] characters and never wrapped, as the
/// target of a click and a double click, with the full first prompt as its tooltip. It is
/// drawn in `ink`, in the medium weight when `strong`.
fn title_text<'a>(session: &SessionNode, strong: bool, ink: Ink) -> Element<'a, Message> {
    let font = if strong { theme::UI_MEDIUM } else { theme::UI };
    let label = text(keep_start(&session.title, TITLE_BUDGET))
        .font(font)
        .style(style::ink(ink))
        .wrapping(Wrapping::None);
    let title: Element<'a, Message> = if session.first_prompt.is_empty() {
        label.into()
    } else {
        tooltip(
            label,
            container(text(session.first_prompt.clone()).size(theme::SMALL))
                .max_width(PROMPT_TIP_WIDTH)
                .padding([4, 8])
                .style(style::card),
            tooltip::Position::Bottom,
        )
        .into()
    };
    title_target(session, title)
}

/// `title`, the part of a row that names its session, as a target of its own: one click
/// moves the cursor onto the row, a double click edits the title.
fn title_target<'a>(session: &SessionNode, title: Element<'a, Message>) -> Element<'a, Message> {
    mouse_area(title)
        .on_press(Message::Projects(ProjectsMessage::Highlight(
            session.id.clone(),
        )))
        .on_double_click(Message::Projects(ProjectsMessage::StartRename(
            session.id.clone(),
        )))
        .into()
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
        format!("{} {}", child.scope_label, badge::status_word(child.status))
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
        ProjectsMessage::ToggleArchived(project.id.clone()),
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
        ProjectsMessage::Select(session.id.clone()),
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
        ProjectsMessage::NewSession(project.id.clone(), Scope::Project),
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
        ProjectsMessage::OpenFolder,
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
    on_press: ProjectsMessage,
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
        .on_press(Message::Projects(on_press))
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
/// of `tree` run and wait for the user, then how many are idle, done and failed joined by
/// dots, then, dim, how many are archived, every count only when there is one; on the right
/// the keys.
fn status_line<'a>(tree: &TreeModel) -> Element<'a, Message> {
    let count = |status: Status| -> usize {
        tree.projects
            .iter()
            .map(|project| sessions_showing(&project.sessions, status))
            .sum()
    };
    let dotted: [(Status, &str, Ink); 2] = [
        (Status::Running, "running", |tokens| tokens.running.fg),
        (Status::WaitingForYou, "asks you", |tokens| {
            tokens.asks_you.fg
        }),
    ];
    let gates: usize = tree
        .projects
        .iter()
        .map(|project| gates_waiting(&project.sessions))
        .sum();
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
    let settled: Vec<Element<'a, Message>> = [
        (Status::Idle, "idle"),
        (Status::Done, "done"),
        (Status::Failed, "failed"),
    ]
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
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::fs;

    use futures::channel::mpsc;
    use iced::Rectangle;
    use iced::widget::Id;
    use iced_test::selector::Candidate;
    use iced_test::{Simulator, simulator};
    use partitur_core::Store;
    use partitur_core::testing::{Call, MemoryGit, MemoryStore, RecordingBackend, single_claude};
    use partitur_core::{
        App, BackendEvent, ExecutionMode, GlobalSettings, PartiturTool, PermissionId,
        PermissionKind, PermissionOption, PermissionOptionId, PlannedTask, ProjectId, RepositoryId,
        Scope, SessionId, StopReason, SuperpowersEvent, ToolCallId, ToolKind, Update,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::app::{BareKey, Message, State, Tab, boot, update, view};
    use crate::new_session_sheet::NewSessionMessage;
    use crate::session_view::RowKey;
    use crate::test_support::{
        Orchestrated, attach, deliver, deliver_tool, orchestrated, orchestrated_after, shown,
        state_after_interrupted_turn,
    };

    /// A project folder with the repository `api`, both with an `AGENTS.md`.
    fn project_dir() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("api").join(".git")).unwrap();
        fs::write(dir.path().join("AGENTS.md"), "global").unwrap();
        fs::write(dir.path().join("api").join("AGENTS.md"), "api only").unwrap();
        dir
    }

    /// A project folder that is itself a repository, so its only repository is its root.
    fn root_repository_dir() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        dir
    }

    /// A state without projects, and the backend that records what it is asked.
    fn empty_state() -> (State, RecordingBackend) {
        let backend = RecordingBackend::new();
        let (backends, default) = single_claude();
        let app = App::load(
            Box::new(backend.clone()),
            Box::new(MemoryStore::new()),
            Box::new(MemoryGit::new()),
            backends,
            default,
            GlobalSettings::default(),
        )
        .unwrap();
        (
            boot(app, mpsc::unbounded().1, mpsc::unbounded().1).0,
            backend,
        )
    }

    /// A state with the project of `dir` open and one session of `scope` in it.
    fn state_with_session(dir: &TempDir, scope: Scope) -> (State, ProjectId, SessionId) {
        let (mut state, _backend) = empty_state();
        let project = state.app.open_project(dir.path()).unwrap();
        let session = state.app.new_session(project.clone(), scope).unwrap();
        (state, project, session)
    }

    /// A state with the project of `dir` open (the way the folder picker opens it) and two
    /// project sessions, oldest first, plus the backend that recorded them.
    fn state_with_project(dir: &TempDir) -> (State, RecordingBackend, [SessionId; 2]) {
        let (mut state, backend) = empty_state();
        apply(
            &mut state,
            [ProjectsMessage::FolderPicked(Some(
                dir.path().to_path_buf(),
            ))],
        );
        let project = state.app.tree().projects[0].id.clone();
        let first = state
            .app
            .new_session(project.clone(), Scope::Project)
            .unwrap();
        let second = state.app.new_session(project, Scope::Project).unwrap();
        (state, backend, [first, second])
    }

    /// The orchestrator of [`orchestrated`] with its child, and a second project (the folder
    /// `dir`) with one session, which is returned. Top to bottom the tree shows the
    /// orchestrator, its child and that session.
    fn orchestrated_and_another_project(dir: &TempDir) -> (Orchestrated, SessionId) {
        let mut fixture = orchestrated();
        let project = fixture.state.app.open_project(dir.path()).unwrap();
        let session = fixture
            .state
            .app
            .new_session(project, Scope::Project)
            .unwrap();
        (fixture, session)
    }

    /// Shows the child of `fixture`'s orchestrator, which starts folded.
    fn expand(fixture: &mut Orchestrated) {
        fixture
            .state
            .app
            .toggle_children(fixture.orchestrator.clone())
            .unwrap();
    }

    /// Feeds the messages through `update`, dropping the tasks: the simulator cannot run them.
    fn apply(state: &mut State, messages: impl IntoIterator<Item = ProjectsMessage>) {
        for message in messages {
            let _task = update(state, crate::app::Message::Projects(message));
        }
    }

    /// Creates the session the new-session sheet prepares, as its Create button does.
    fn create(state: &mut State) {
        let _task = update(
            state,
            crate::app::Message::NewSession(NewSessionMessage::Create),
        );
    }

    fn click(state: &State, id: String) -> Vec<crate::app::Message> {
        let mut ui = simulator(view(state));
        ui.click(Id::from(id)).unwrap();
        ui.into_messages().collect()
    }

    /// Where the widget found by `id` is.
    fn bounds_of(ui: &mut Simulator<'_, crate::app::Message>, id: String) -> Rectangle {
        ui.find(Id::from(id)).unwrap().bounds()
    }

    /// Whether the text `label` is shown inside `area`.
    fn shows_within(
        ui: &mut Simulator<'_, crate::app::Message>,
        label: &str,
        area: Rectangle,
    ) -> bool {
        ui.find(move |candidate: Candidate<'_>| match candidate {
            Candidate::Text {
                content, bounds, ..
            } if content == label && area.contains(bounds.center()) => Some(()),
            _ => None,
        })
        .is_ok()
    }

    /// The update that ends a session's turn the regular way.
    fn turn_ended(session: &SessionId) -> BackendEvent {
        BackendEvent::Update {
            session: session.clone(),
            update: Update::TurnEnded(StopReason::EndTurn),
        }
    }

    #[test]
    fn an_open_project_lists_its_sessions() {
        let dir = project_dir();
        let (state, _backend, [first, second]) = state_with_project(&dir);
        let project = state.app.tree().projects[0].clone();
        let folder = home_path::abbreviated(&project.root, std::env::home_dir().as_deref());
        let mut ui = simulator(view(&state));

        ui.find(project.name.as_str()).unwrap();
        ui.find(folder.as_str()).unwrap();
        ui.find("1 repo").unwrap();
        ui.find("new session").unwrap();
        ui.find(Id::from(format!("session-{first}"))).unwrap();
        ui.find(Id::from(format!("session-{second}"))).unwrap();
        ui.find(Id::from(format!("new-session-{}", project.id)))
            .unwrap();
        ui.find("open project folder").unwrap();
        assert!(ui.find("No projects open").is_err());
    }

    #[test]
    fn a_root_claude_md_is_named_in_the_project_header() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("api").join(".git")).unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "global").unwrap();
        let (state, _backend, _) = state_with_project(&dir);
        let project = state.app.tree().projects[0].id.clone();

        let mut ui = simulator(view(&state));
        let header = bounds_of(&mut ui, format!("project-{project}"));
        assert!(shows_within(&mut ui, "CLAUDE.md", header));
        assert!(ui.find("AGENTS.md").is_err());
    }

    #[test]
    fn a_repository_claude_md_is_not_named_in_the_tree() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("api").join(".git")).unwrap();
        fs::write(dir.path().join("api").join("CLAUDE.md"), "api only").unwrap();
        let (state, _backend, _) = state_with_project(&dir);

        let mut ui = simulator(view(&state));
        assert!(ui.find("CLAUDE.md").is_err());
        assert!(ui.find("AGENTS.md").is_err());
    }

    #[test]
    fn a_project_with_both_instruction_files_names_both_in_its_header() {
        let dir = project_dir();
        fs::write(dir.path().join("CLAUDE.md"), "global").unwrap();
        let (state, _backend, _) = state_with_project(&dir);
        let project = state.app.tree().projects[0].id.clone();

        let mut ui = simulator(view(&state));
        let header = bounds_of(&mut ui, format!("project-{project}"));
        assert!(shows_within(&mut ui, "AGENTS.md", header));
        assert!(shows_within(&mut ui, "CLAUDE.md", header));
    }

    #[test]
    fn the_header_reads_name_repos_path_then_instruction_files() {
        let dir = TempDir::new().unwrap();
        for name in ["api", "web"] {
            fs::create_dir_all(dir.path().join(name).join(".git")).unwrap();
        }
        fs::write(dir.path().join("CLAUDE.md"), "global").unwrap();
        let (state, _backend, _) = state_with_project(&dir);
        let project = state.app.tree().projects[0].clone();
        let folder = keep_end(
            &home_path::abbreviated(&project.root, std::env::home_dir().as_deref()),
            PATH_BUDGET,
        );

        let mut ui = simulator(view(&state));
        let name = ui.find(project.name.as_str()).unwrap().bounds();
        let repos = ui.find("2 repos").unwrap().bounds();
        let path = ui.find(folder.as_str()).unwrap().bounds();
        let file = ui.find("CLAUDE.md").unwrap().bounds();
        assert!(name.x < repos.x);
        assert!(repos.x < path.x);
        assert!(path.x < file.x);
    }

    #[test]
    fn the_close_button_shows_only_while_the_header_is_hovered() {
        let dir = project_dir();
        let (mut state, _backend, _) = state_with_project(&dir);
        let project = state.app.tree().projects[0].id.clone();
        let close = Id::from(format!("close-{project}"));
        assert!(simulator(view(&state)).find(close.clone()).is_err());

        apply(&mut state, [ProjectsMessage::HoverProject(project.clone())]);
        simulator(view(&state)).find(close.clone()).unwrap();

        apply(&mut state, [ProjectsMessage::UnhoverProject(project)]);
        assert!(simulator(view(&state)).find(close).is_err());
    }

    #[test]
    fn moving_the_pointer_onto_a_project_header_hovers_it() {
        let dir = project_dir();
        let (state, _backend, _) = state_with_project(&dir);
        let project = state.app.tree().projects[0].id.clone();

        let mut ui = simulator(view(&state));
        let header = bounds_of(&mut ui, format!("project-{project}"));
        ui.point_at(header.center());
        ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
            position: header.center(),
        })]);
        let messages: Vec<_> = ui.into_messages().collect();
        assert!(messages.contains(&crate::app::Message::Projects(
            ProjectsMessage::HoverProject(project)
        )));
    }

    #[test]
    fn an_unhover_of_another_project_keeps_the_hovered_header() {
        let dir = project_dir();
        let (mut state, _backend, _) = state_with_project(&dir);
        let project = state.app.tree().projects[0].id.clone();

        apply(
            &mut state,
            [
                ProjectsMessage::HoverProject(project.clone()),
                ProjectsMessage::UnhoverProject(ProjectId::new()),
            ],
        );
        assert_eq!(state.projects.hovered_project, Some(project));
    }

    #[test]
    fn collapsing_a_project_hides_its_rows() {
        let dir = project_dir();
        let (mut fixture, other) = orchestrated_and_another_project(&dir);
        expand(&mut fixture);
        let project = fixture.state.app.tree().projects[0].id.clone();
        fixture.state.tab = Tab::Projects;

        let messages = click(&fixture.state, format!("project-toggle-{project}"));
        assert_eq!(
            messages,
            [crate::app::Message::Projects(
                ProjectsMessage::ToggleProject(project.clone())
            )]
        );
        for message in messages {
            let _task = update(&mut fixture.state, message);
        }
        assert_eq!(fixture.state.error, None);
        {
            let mut ui = simulator(view(&fixture.state));
            ui.find(Id::from(format!("project-{project}"))).unwrap();
            for hidden in [&fixture.orchestrator, &fixture.child] {
                assert!(ui.find(Id::from(format!("session-{hidden}"))).is_err());
            }
            assert!(ui.find(Id::from(format!("new-session-{project}"))).is_err());
            ui.find(Id::from(format!("session-{other}"))).unwrap();
        }

        // The cursor skips the folded project's sessions.
        fixture.state.projects.cursor = None;
        apply(&mut fixture.state, [ProjectsMessage::CursorDown]);
        assert_eq!(fixture.state.projects.cursor, Some(other.clone()));
        apply(&mut fixture.state, [ProjectsMessage::CursorUp]);
        assert_eq!(fixture.state.projects.cursor, Some(other));

        apply(
            &mut fixture.state,
            [ProjectsMessage::ToggleProject(project.clone())],
        );
        let mut ui = simulator(view(&fixture.state));
        ui.find(Id::from(format!("session-{}", fixture.child)))
            .unwrap();
        ui.find(Id::from(format!("new-session-{project}"))).unwrap();
    }

    #[test]
    fn collapsing_a_project_takes_the_cursor_off_its_rows() {
        let mut fixture = orchestrated();
        let project = fixture.state.app.tree().projects[0].id.clone();
        fixture.state.projects.cursor = Some(fixture.child.clone());

        apply(
            &mut fixture.state,
            [ProjectsMessage::ToggleProject(project)],
        );
        assert_eq!(fixture.state.projects.cursor, None);
    }

    #[test]
    fn collapsing_a_project_keeps_a_cursor_in_another_project() {
        let dir = project_dir();
        let (mut fixture, other) = orchestrated_and_another_project(&dir);
        let project = fixture.state.app.tree().projects[0].id.clone();
        fixture.state.projects.cursor = Some(other.clone());

        apply(
            &mut fixture.state,
            [ProjectsMessage::ToggleProject(project)],
        );
        assert_eq!(fixture.state.projects.cursor, Some(other));
    }

    #[test]
    fn collapsing_an_unknown_project_is_reported() {
        let (mut state, _backend) = empty_state();

        apply(
            &mut state,
            [ProjectsMessage::ToggleProject(ProjectId::new())],
        );
        assert!(state.error.is_some());
    }

    #[test]
    fn a_collapsed_project_points_its_chevron_right() {
        let dir = project_dir();
        let (mut state, _backend, _) = state_with_project(&dir);
        let project = state.app.tree().projects[0].id.clone();
        assert_eq!(header_chevron(&state, &project), Icon::ChevronDown);

        apply(
            &mut state,
            [ProjectsMessage::ToggleProject(project.clone())],
        );
        assert_eq!(header_chevron(&state, &project), Icon::ChevronRight);
    }

    /// The icon of `project`'s header chevron.
    fn header_chevron(state: &State, project: &ProjectId) -> Icon {
        let tree = state.app.tree();
        let node = tree
            .projects
            .iter()
            .find(|open| &open.id == project)
            .unwrap();
        fold_icon(node.fold)
    }

    #[test]
    fn the_content_is_centred_at_1080() {
        let dir = project_dir();
        let (state, _backend, _) = state_with_project(&dir);
        let project = state.app.tree().projects[0].id.clone();
        let text_edge = |width: f32| {
            let mut ui = Simulator::with_size(
                crate::settings(),
                iced::Size::new(width, 800.0),
                view(&state),
            );
            // The chevron's icon sits in the middle of its slot, at the text edge.
            bounds_of(&mut ui, format!("project-toggle-{project}")).x + SLOT_REACH
        };

        let wide = text_edge(1600.0);
        assert!((wide - (1600.0 - CONTENT_WIDTH) / 2.0).abs() <= BLEED);
        assert!((wide - ((1600.0 - CONTENT_WIDTH) / 2.0 + BLEED)).abs() < 0.5);
        assert!((text_edge(900.0) - TREE_MARGIN_X).abs() < 0.5);
    }

    #[test]
    fn a_repository_session_names_its_repository_after_its_title() {
        let dir = project_dir();
        let api = RepositoryId::try_from("api").unwrap();
        let (state, _, session) = state_with_session(&dir, Scope::Repository(api));

        let mut ui = simulator(view(&state));
        let row = bounds_of(&mut ui, format!("session-{session}"));
        let title = ui.find("untitled").unwrap().bounds();
        let repository = ui.find("api").unwrap().bounds();
        assert!(row.contains(repository.center()));
        assert!(repository.x > title.x);
    }

    #[test]
    fn a_project_session_names_no_repository() {
        let dir = project_dir();
        let (state, _, session) = state_with_session(&dir, Scope::Project);

        let mut ui = simulator(view(&state));
        ui.find(Id::from(format!("session-{session}"))).unwrap();
        assert!(ui.find("api").is_err());
    }

    #[test]
    fn a_spawned_session_leads_with_its_repository() {
        let mut fixture = orchestrated();
        expand(&mut fixture);

        let mut ui = simulator(view(&fixture.state));
        let row = bounds_of(&mut ui, format!("session-{}", fixture.child));
        let repository = ui.find("api").unwrap().bounds();
        let title = ui.find("API endpoint").unwrap().bounds();
        assert!(row.contains(repository.center()));
        assert!(row.contains(title.center()));
        assert!(repository.x < title.x);
    }

    #[test]
    fn the_orchestrator_pill_marks_only_orchestrators() {
        let mut fixture = orchestrated();
        expand(&mut fixture);
        let mut ui = simulator(view(&fixture.state));
        let orchestrator = bounds_of(&mut ui, format!("session-{}", fixture.orchestrator));
        let child = bounds_of(&mut ui, format!("session-{}", fixture.child));
        assert!(shows_within(&mut ui, badge::ORCHESTRATOR, orchestrator));
        assert!(!shows_within(&mut ui, badge::ORCHESTRATOR, child));

        let dir = project_dir();
        let (state, _backend, _) = state_with_project(&dir);
        let mut ui = simulator(view(&state));
        assert!(ui.find(badge::ORCHESTRATOR).is_err());
    }

    #[test]
    fn an_idle_session_shows_an_idle_pill() {
        let dir = project_dir();
        let (state, _backend, [first, _]) = state_with_project(&dir);

        let mut ui = simulator(view(&state));
        let row = bounds_of(&mut ui, format!("session-{first}"));
        assert!(shows_within(&mut ui, "idle", row));
        assert!(ui.find(Id::from(format!("toggle-{first}"))).is_err());
    }

    #[test]
    fn a_project_that_is_itself_a_repository_offers_one_new_session_row() {
        let dir = root_repository_dir();
        let (state, project, _) = state_with_session(&dir, Scope::Project);
        let root = dir.path().file_name().unwrap().to_string_lossy();

        let mut ui = simulator(view(&state));
        ui.find(Id::from(format!("new-session-{project}"))).unwrap();
        ui.find("new session").unwrap();
        ui.find("1 repo").unwrap();
        assert!(
            ui.find(Id::from(format!("new-session-{project}-{root}")))
                .is_err()
        );
        assert!(ui.find(format!("in {root}").as_str()).is_err());
        assert!(ui.find(NEW_SESSION_HINT).is_err());
    }

    #[test]
    fn a_multi_repo_project_offers_one_new_session_row_with_a_hint() {
        let fixture = orchestrated();
        let project = fixture.state.app.tree().projects[0].id.clone();

        let mut ui = simulator(view(&fixture.state));
        let row = bounds_of(&mut ui, format!("new-session-{project}"));
        ui.find("2 repos").unwrap();
        assert!(
            ui.find(Id::from(format!("new-session-{project}-api")))
                .is_err()
        );
        assert!(ui.find("in api").is_err());
        assert!(ui.find("in web").is_err());
        assert!(shows_within(&mut ui, "new session", row));
        assert!(shows_within(
            &mut ui,
            "orchestrator or a single repo — chosen in the sheet",
            row
        ));
    }

    #[test]
    fn a_project_with_one_repository_besides_its_root_has_no_hint() {
        let dir = project_dir();
        let (state, _backend, _) = state_with_project(&dir);

        let mut ui = simulator(view(&state));
        assert!(ui.find(NEW_SESSION_HINT).is_err());
    }

    #[test]
    fn the_repository_count_is_singular_for_one() {
        assert_eq!(repository_count(0), "0 repos");
        assert_eq!(repository_count(1), "1 repo");
        assert_eq!(repository_count(3), "3 repos");
    }

    #[test]
    fn only_a_session_with_children_carries_a_chevron() {
        let fixture = orchestrated();

        let mut ui = simulator(view(&fixture.state));
        ui.find(Id::from(format!("toggle-{}", fixture.orchestrator)))
            .unwrap();
        assert!(
            ui.find(Id::from(format!("toggle-{}", fixture.child)))
                .is_err()
        );
    }

    #[test]
    fn folding_hides_the_child_rows_and_expanding_shows_them() {
        let mut fixture = orchestrated();
        expand(&mut fixture);
        let toggle = format!("toggle-{}", fixture.orchestrator);
        let child = Id::from(format!("session-{}", fixture.child));

        let messages = click(&fixture.state, toggle.clone());
        assert_eq!(
            messages,
            [crate::app::Message::Projects(
                ProjectsMessage::ToggleChildren(fixture.orchestrator.clone())
            )]
        );
        for message in messages {
            let _task = update(&mut fixture.state, message);
        }
        assert_eq!(fixture.state.tab, Tab::Projects);
        {
            let mut ui = simulator(view(&fixture.state));
            assert!(ui.find(child.clone()).is_err());
            ui.find(Id::from(format!("session-{}", fixture.orchestrator)))
                .unwrap();
        }

        for message in click(&fixture.state, toggle) {
            let _task = update(&mut fixture.state, message);
        }
        let mut ui = simulator(view(&fixture.state));
        ui.find(child).unwrap();
    }

    #[test]
    fn folding_a_row_above_the_cursor_moves_the_cursor_onto_it() {
        let mut fixture = orchestrated();
        expand(&mut fixture);
        fixture.state.projects.cursor = Some(fixture.child.clone());

        apply(
            &mut fixture.state,
            [ProjectsMessage::ToggleChildren(
                fixture.orchestrator.clone(),
            )],
        );
        assert_eq!(
            fixture.state.projects.cursor,
            Some(fixture.orchestrator.clone())
        );

        apply(
            &mut fixture.state,
            [ProjectsMessage::ToggleChildren(
                fixture.orchestrator.clone(),
            )],
        );
        assert_eq!(fixture.state.projects.cursor, Some(fixture.orchestrator));
    }

    #[test]
    fn selecting_a_session_expands_the_folded_sessions_above_it() {
        let mut fixture = orchestrated();
        let project = fixture.state.app.tree().projects[0].id.clone();
        let other = fixture
            .state
            .app
            .new_session(project, Scope::Project)
            .unwrap();
        fixture.state.tab = Tab::Projects;
        let child_row = Id::from(format!("session-{}", fixture.child));
        let mut ui = simulator(view(&fixture.state));
        assert!(ui.find(child_row.clone()).is_err());
        drop(ui);

        // The child is picked somewhere else: in the jump panel, on its card.
        apply(
            &mut fixture.state,
            [ProjectsMessage::Select(fixture.child.clone())],
        );

        assert_eq!(fixture.state.error, None);
        assert!(node(&fixture.state, &fixture.orchestrator).expanded);
        // Folded, but not above the child: it stays as it is.
        assert!(!node(&fixture.state, &other).expanded);
        assert!(!node(&fixture.state, &fixture.child).expanded);
        assert_eq!(fixture.state.projects.cursor, Some(fixture.child.clone()));
        fixture.state.tab = Tab::Projects;
        let mut ui = simulator(view(&fixture.state));
        ui.find(child_row).unwrap();
    }

    #[test]
    fn selecting_a_session_in_a_folded_project_opens_the_project() {
        let dir = project_dir();
        let (mut fixture, other) = orchestrated_and_another_project(&dir);
        let first = fixture.state.app.tree().projects[0].id.clone();
        let second = fixture.state.app.tree().projects[1].id.clone();
        fixture.state.app.toggle_project(first.clone()).unwrap();
        fixture.state.app.toggle_project(second.clone()).unwrap();

        // The session is picked somewhere else: in the jump panel.
        apply(&mut fixture.state, [ProjectsMessage::Select(other.clone())]);

        assert_eq!(fixture.state.error, None);
        let tree = fixture.state.app.tree();
        assert_eq!(tree.projects[1].fold, Fold::Open);
        // Another folded project stays folded.
        assert_eq!(tree.projects[0].fold, Fold::Folded);
        let saved = fixture.store.load_projects().unwrap();
        let saved_fold = |id: &ProjectId| {
            saved
                .iter()
                .find(|project| &project.id == id)
                .map(|project| project.fold)
        };
        assert_eq!(saved_fold(&second), Some(Fold::Open));
        assert_eq!(saved_fold(&first), Some(Fold::Folded));
        assert_eq!(fixture.state.projects.cursor, Some(other.clone()));
        fixture.state.tab = Tab::Projects;
        let mut ui = simulator(view(&fixture.state));
        ui.find(Id::from(format!("session-{other}"))).unwrap();
    }

    #[test]
    fn selecting_a_session_in_an_open_project_leaves_it_open() {
        let dir = project_dir();
        let (mut fixture, other) = orchestrated_and_another_project(&dir);

        apply(&mut fixture.state, [ProjectsMessage::Select(other)]);

        assert_eq!(fixture.state.app.tree().projects[1].fold, Fold::Open);
    }

    #[test]
    fn the_sessions_above_a_session_are_found_outermost_first() {
        let fixture = orchestrated();
        let tree = fixture.state.app.tree();
        let sessions = &tree.projects[0].sessions;
        let above_of = |target: &SessionId| {
            let mut above = Vec::new();
            let found = sessions_above(sessions, target, &mut above);
            let above: Vec<SessionId> = above.iter().map(|node| node.id.clone()).collect();
            (found, above)
        };

        assert_eq!(
            above_of(&fixture.child),
            (true, vec![fixture.orchestrator.clone()])
        );
        assert_eq!(above_of(&fixture.orchestrator), (true, Vec::new()));
        assert_eq!(above_of(&SessionId::new()), (false, Vec::new()));
    }

    #[test]
    fn rows_sit_at_their_level_under_the_project_header() {
        let mut fixture = orchestrated();
        expand(&mut fixture);
        let project = fixture.state.app.tree().projects[0].clone();

        let mut ui = simulator(view(&fixture.state));
        let chevron = bounds_of(&mut ui, format!("project-toggle-{}", project.id));
        let top = bounds_of(&mut ui, format!("session-{}", fixture.orchestrator));
        let child = bounds_of(&mut ui, format!("session-{}", fixture.child));
        let new_session = bounds_of(&mut ui, format!("new-session-{}", project.id));
        let open_folder = bounds_of(&mut ui, "open-project-folder".to_owned());
        // A row's content starts with its slot, so a top-level session's slot sits under
        // the header's chevron and its title under the project's name.
        assert!((top.x - chevron.x).abs() < 0.5);
        assert!((child.x - chevron.x - LEVEL_INDENT).abs() < 0.5);
        assert!((new_session.x - chevron.x - SESSION_INDENT).abs() < 0.5);
        assert!((open_folder.x - chevron.x).abs() < 0.5);
        assert!((top.height - ROW_HEIGHT).abs() < 0.5);
        let name = ui.find(project.name.as_str()).unwrap().bounds();
        let title = ui.find("untitled").unwrap().bounds();
        assert!((name.x - title.x).abs() < 0.5);
    }

    #[test]
    fn without_projects_the_tab_offers_to_open_one() {
        let (state, _backend) = empty_state();
        let mut ui = simulator(view(&state));

        ui.find("No projects open").unwrap();
        ui.click("open project folder").unwrap();

        let messages: Vec<_> = ui.into_messages().collect();
        assert_eq!(
            messages,
            [crate::app::Message::Projects(ProjectsMessage::OpenFolder)]
        );
    }

    #[test]
    fn clicking_a_session_selects_it_and_switches_to_the_session_tab() {
        let dir = project_dir();
        let (mut state, _backend, [first, second]) = state_with_project(&dir);
        assert_eq!(state.app.current_session(), Some(second));
        assert_eq!(state.tab, Tab::Projects);

        let messages = click(&state, format!("session-{first}"));

        assert_eq!(
            messages,
            [crate::app::Message::Projects(ProjectsMessage::Select(
                first.clone()
            ))]
        );
        for message in messages {
            let _task = update(&mut state, message);
        }
        assert_eq!(state.tab, Tab::Session);
        assert_eq!(state.app.current_session(), Some(first));
    }

    #[test]
    fn the_cursor_follows_the_selection() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        let project = state.app.tree().projects[0].id.clone();
        assert_eq!(state.projects.cursor, None);

        apply(&mut state, [ProjectsMessage::Select(first.clone())]);
        assert_eq!(state.projects.cursor, Some(first.clone()));

        apply(
            &mut state,
            [ProjectsMessage::NewSession(project, Scope::Project)],
        );
        create(&mut state);
        let newest = state.app.current_session().unwrap();
        assert_ne!(newest, first);
        assert_eq!(state.projects.cursor, Some(newest));
    }

    #[test]
    fn the_cursor_starts_on_the_current_session() {
        let dir = project_dir();
        let (backends, default) = single_claude();
        let mut app = App::load(
            Box::new(RecordingBackend::new()),
            Box::new(MemoryStore::new()),
            Box::new(MemoryGit::new()),
            backends,
            default,
            GlobalSettings::default(),
        )
        .unwrap();
        let project = app.open_project(dir.path()).unwrap();
        let session = app.new_session(project, Scope::Project).unwrap();

        let state = boot(app, mpsc::unbounded().1, mpsc::unbounded().1).0;

        assert_eq!(state.projects.cursor, Some(session));
    }

    #[test]
    fn a_session_that_cannot_be_selected_leaves_the_cursor_where_it_is() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        state.projects.cursor = Some(first.clone());

        apply(&mut state, [ProjectsMessage::Select(SessionId::new())]);

        assert!(state.error.is_some());
        assert_eq!(state.projects.cursor, Some(first));
        assert_eq!(state.tab, Tab::Projects);
    }

    #[test]
    fn the_arrow_keys_move_the_cursor_over_the_visible_rows() {
        let dir = project_dir();
        let (mut fixture, other) = orchestrated_and_another_project(&dir);
        expand(&mut fixture);
        let (orchestrator, child) = (fixture.orchestrator.clone(), fixture.child.clone());
        let state = &mut fixture.state;
        let mut cursor_after = |message| {
            apply(state, [message]);
            state.projects.cursor.clone()
        };

        // Without a cursor, up has nowhere to start from and down starts at the top.
        assert_eq!(cursor_after(ProjectsMessage::CursorUp), None);
        assert_eq!(
            cursor_after(ProjectsMessage::CursorDown),
            Some(orchestrator.clone())
        );
        assert_eq!(
            cursor_after(ProjectsMessage::CursorDown),
            Some(child.clone())
        );
        assert_eq!(
            cursor_after(ProjectsMessage::CursorDown),
            Some(other.clone())
        );
        // The last row is the end: the cursor does not wrap around.
        assert_eq!(
            cursor_after(ProjectsMessage::CursorDown),
            Some(other.clone())
        );
        assert_eq!(cursor_after(ProjectsMessage::CursorUp), Some(child));
        assert_eq!(
            cursor_after(ProjectsMessage::CursorUp),
            Some(orchestrator.clone())
        );
        assert_eq!(
            cursor_after(ProjectsMessage::CursorUp),
            Some(orchestrator.clone())
        );

        // A collapsed session's children are skipped in both directions.
        assert_eq!(
            cursor_after(ProjectsMessage::ToggleChildren(orchestrator.clone())),
            Some(orchestrator.clone())
        );
        assert_eq!(
            cursor_after(ProjectsMessage::CursorDown),
            Some(other.clone())
        );
        assert_eq!(cursor_after(ProjectsMessage::CursorUp), Some(orchestrator));
        assert_eq!(state.tab, Tab::Projects);
    }

    #[test]
    fn the_bare_keys_drive_the_tree_while_nothing_covers_it() {
        use crate::app::{BareKey, Message};

        let dir = project_dir();
        let (mut state, _backend, [first, second]) = state_with_project(&dir);
        let press = |state: &mut State, key| {
            let _task = update(state, Message::Key(key));
        };

        // The newest session is the first row.
        press(&mut state, BareKey::ArrowDown);
        assert_eq!(state.projects.cursor, Some(second.clone()));
        press(&mut state, BareKey::ArrowDown);
        assert_eq!(state.projects.cursor, Some(first.clone()));
        press(&mut state, BareKey::ArrowUp);
        assert_eq!(state.projects.cursor, Some(second));
        press(&mut state, BareKey::ArrowDown);
        press(&mut state, BareKey::Enter);
        assert_eq!(state.app.current_session(), Some(first));
        assert_eq!(state.tab, Tab::Session);

        state.tab = Tab::Projects;
        press(&mut state, BareKey::N);
        press(&mut state, BareKey::Enter);
        assert_eq!(state.app.tree().projects[0].sessions.len(), 3);
    }

    #[test]
    fn enter_opens_the_cursors_session() {
        let dir = project_dir();
        let (mut state, _backend, [first, second]) = state_with_project(&dir);

        // Without a cursor there is nothing to open.
        apply(&mut state, [ProjectsMessage::OpenCursor]);
        assert_eq!(state.tab, Tab::Projects);
        assert_eq!(state.app.current_session(), Some(second));

        state.projects.cursor = Some(first.clone());
        apply(&mut state, [ProjectsMessage::OpenCursor]);
        assert_eq!(state.tab, Tab::Session);
        assert_eq!(state.app.current_session(), Some(first));
    }

    #[test]
    fn n_starts_a_project_session_in_the_cursors_project() {
        let dir = project_dir();
        let (mut fixture, other) = orchestrated_and_another_project(&dir);
        let state = &mut fixture.state;
        let session_counts = |state: &State| -> Vec<usize> {
            let tree = state.app.tree();
            tree.projects
                .iter()
                .map(|project| project.sessions.len())
                .collect()
        };
        assert_eq!(session_counts(state), [1, 1]);

        // Without a cursor the session goes to the first project.
        apply(state, [ProjectsMessage::NewSessionAtCursor]);
        create(state);
        assert_eq!(session_counts(state), [2, 1]);
        assert_eq!(state.tab, Tab::Session);
        let created = state.app.current_session().unwrap();
        assert_eq!(state.projects.cursor, Some(created));

        state.tab = Tab::Projects;
        state.projects.cursor = Some(other);
        apply(state, [ProjectsMessage::NewSessionAtCursor]);
        create(state);
        assert_eq!(session_counts(state), [2, 2]);
        let tree = state.app.tree();
        let newest = &tree.projects[1].sessions[0];
        assert_eq!(Some(newest.id.clone()), state.app.current_session());
        assert_eq!(newest.scope_label, "");

        // A cursor on a spawned session still names its project.
        state.tab = Tab::Projects;
        state.projects.cursor = Some(fixture.child.clone());
        apply(state, [ProjectsMessage::NewSessionAtCursor]);
        create(state);
        assert_eq!(session_counts(state), [3, 2]);
    }

    #[test]
    fn n_without_a_project_does_nothing() {
        let (mut state, _backend) = empty_state();

        apply(&mut state, [ProjectsMessage::NewSessionAtCursor]);

        assert_eq!(state.error, None);
        assert_eq!(state.tab, Tab::Projects);
        assert_eq!(state.app.current_session(), None);
    }

    #[test]
    fn the_tree_keys_do_nothing_on_another_tab() {
        let dir = project_dir();
        let (mut state, backend, [first, second]) = state_with_project(&dir);
        state.projects.cursor = Some(first.clone());
        let calls = backend.calls().len();

        for tab in [Tab::Session, Tab::Env] {
            state.tab = tab;
            apply(
                &mut state,
                [
                    ProjectsMessage::CursorDown,
                    ProjectsMessage::CursorUp,
                    ProjectsMessage::OpenCursor,
                    ProjectsMessage::NewSessionAtCursor,
                ],
            );

            assert_eq!(state.tab, tab);
            assert_eq!(state.projects.cursor, Some(first.clone()));
            assert_eq!(state.app.current_session(), Some(second.clone()));
            assert_eq!(state.app.tree().projects[0].sessions.len(), 2);
            assert_eq!(backend.calls().len(), calls);
        }
    }

    #[test]
    fn new_session_for_the_project_runs_in_the_project_root() {
        let dir = project_dir();
        let (mut state, backend, _) = state_with_project(&dir);
        let project = state.app.tree().projects[0].id.clone();

        let messages = click(&state, format!("new-session-{project}"));
        for message in messages {
            let _task = update(&mut state, message);
        }
        create(&mut state);

        let calls = backend.calls();
        let (_, _, cwd) = calls.last().and_then(Call::as_new_session).unwrap();
        assert_eq!(cwd, dir.path());
        assert_eq!(state.tab, Tab::Session);
    }

    /// The update that makes a session ask the user for permission.
    fn asks_permission() -> Update {
        Update::PermissionRequested {
            id: PermissionId(1),
            tool_call_id: ToolCallId::new("t-1"),
            title: "rm -rf".into(),
            kind: ToolKind::Execute,
            options: vec![PermissionOption {
                id: PermissionOptionId::new("allow"),
                name: "Allow".into(),
                kind: PermissionKind::AllowOnce,
            }],
            name: None,
            locations: Vec::new(),
            partitur_tool: false,
        }
    }

    #[test]
    fn a_waiting_session_shows_the_asks_you_badge() {
        let dir = project_dir();
        let (mut state, backend, [_, second]) = state_with_project(&dir);
        let row_id = format!("session-{second}");
        let request = backend
            .calls()
            .iter()
            .rev()
            .find_map(|call| call.as_new_session().map(|(request, ..)| request))
            .unwrap();
        state
            .app
            .apply_event(BackendEvent::SessionCreated {
                request,
                session: second.clone(),
                backend_session: partitur_core::BackendSessionId::new("b-1"),
                config: Vec::new(),
            })
            .unwrap();
        state
            .app
            .apply_event(BackendEvent::Update {
                session: second,
                update: asks_permission(),
            })
            .unwrap();

        let mut ui = simulator(view(&state));
        let row = bounds_of(&mut ui, row_id);

        assert!(shows_within(&mut ui, "asks you", row));
        assert!(!shows_within(&mut ui, "idle", row));
    }

    #[test]
    fn an_interrupted_session_shows_its_pill_and_notice() {
        let dir = project_dir();
        let mut state = state_after_interrupted_turn(&dir);

        {
            let mut tree = simulator(view(&state));
            assert_eq!(shown(&mut tree, "interrupted"), 1);
            assert!(tree.find("running").is_err());
        }

        state.tab = Tab::Session;
        let mut session = simulator(view(&state));
        session.find("interrupted").unwrap();
        session
            .find("Partitur closed before this turn ended")
            .unwrap();
        assert!(session.find("running").is_err());
    }

    #[test]
    fn a_parent_of_a_waiting_child_is_marked_in_the_tree() {
        let mut fixture = orchestrated();
        expand(&mut fixture);
        deliver(
            &mut fixture.state,
            BackendEvent::Update {
                session: fixture.child.clone(),
                update: asks_permission(),
            },
        );

        {
            let mut ui = simulator(view(&fixture.state));
            let orchestrator = bounds_of(&mut ui, format!("session-{}", fixture.orchestrator));
            let child = bounds_of(&mut ui, format!("session-{}", fixture.child));
            assert!(shows_within(&mut ui, CHILD_ASKS_YOU, orchestrator));
            assert!(shows_within(&mut ui, "running", orchestrator));
            assert!(shows_within(&mut ui, "asks you", child));
            assert!(!shows_within(&mut ui, CHILD_ASKS_YOU, child));
        }

        fixture
            .state
            .app
            .select_session(fixture.orchestrator.clone())
            .unwrap();
        fixture.state.tab = Tab::Session;
        let mut ui = simulator(view(&fixture.state));
        ui.find("child asks you").unwrap();
    }

    #[test]
    fn a_done_parent_with_a_running_child_reads_running() {
        let mut fixture = orchestrated();
        let orchestrator = fixture.orchestrator.clone();
        deliver(&mut fixture.state, turn_ended(&orchestrator));

        let tree = fixture.state.app.tree();
        let parent = &tree.projects[0].sessions[0];
        assert_eq!(parent.id, fixture.orchestrator);
        assert_eq!(parent.status, Status::Running);
        let mut ui = simulator(view(&fixture.state));
        ui.find("running").unwrap();
        assert!(ui.find("done").is_err());
        assert!(ui.find("child asks you").is_err());
    }

    #[test]
    fn every_session_shows_its_context_usage_once_known() {
        let mut fixture = orchestrated();
        expand(&mut fixture);
        let child = fixture.child.clone();
        {
            let mut ui = simulator(view(&fixture.state));
            assert!(ui.find("41k / 200k").is_err());
        }

        for session in [&fixture.orchestrator, &fixture.child] {
            deliver(
                &mut fixture.state,
                BackendEvent::Update {
                    session: session.clone(),
                    update: Update::Usage {
                        used: 41_260,
                        size: 200_000,
                    },
                },
            );
        }

        // An orchestrator's row tells its own usage, as every other row does.
        let mut ui = simulator(view(&fixture.state));
        let row = bounds_of(&mut ui, format!("session-{child}"));
        assert!(shows_within(&mut ui, "41k / 200k", row));
        let orchestrator = bounds_of(&mut ui, format!("session-{}", fixture.orchestrator));
        assert!(shows_within(&mut ui, "41k / 200k", orchestrator));
        assert!(ui.find("0 of 1 children done").is_err());
    }

    #[test]
    fn the_status_line_counts_the_sessions_by_status_at_any_depth() {
        let dir = project_dir();
        let (mut fixture, _idle) = orchestrated_and_another_project(&dir);
        let (orchestrator, child) = (fixture.orchestrator.clone(), fixture.child.clone());
        {
            let mut ui = simulator(view(&fixture.state));
            ui.find("2 running").unwrap();
            ui.find("1 idle").unwrap();
            for absent in ["asks you", "failed", "done"] {
                assert!(ui.find(format!("0 {absent}").as_str()).is_err());
                assert!(ui.find(format!("1 {absent}").as_str()).is_err());
            }
        }

        deliver(
            &mut fixture.state,
            BackendEvent::Update {
                session: child.clone(),
                update: asks_permission(),
            },
        );
        {
            let mut ui = simulator(view(&fixture.state));
            ui.find("1 running").unwrap();
            ui.find("1 asks you").unwrap();
        }

        // The failed child's result starts another turn of the orchestrator, which ends too.
        deliver(
            &mut fixture.state,
            BackendEvent::Update {
                session: child,
                update: Update::Failed("the agent crashed".into()),
            },
        );
        deliver(&mut fixture.state, turn_ended(&orchestrator));
        {
            let mut ui = simulator(view(&fixture.state));
            ui.find("1 running").unwrap();
            ui.find("1 failed").unwrap();
            assert!(ui.find("1 asks you").is_err());
        }
        deliver(&mut fixture.state, turn_ended(&orchestrator));
        let mut ui = simulator(view(&fixture.state));
        ui.find("1 done").unwrap();
        ui.find("1 failed").unwrap();
        assert!(ui.find("1 running").is_err());
    }

    /// Lets `session` of `state` ask for approval of its design, then waits for it.
    fn waits_at_the_design(state: &mut State, backend: &RecordingBackend, session: &SessionId) {
        attach(state, backend, session, &format!("b-{session}"));
        for tool in [
            SuperpowersEvent::StartSession {
                topic: "Worktrees".into(),
            },
            SuperpowersEvent::StartBrainstorming,
            SuperpowersEvent::RequestDesignApproval {
                path: "docs/design.md".into(),
            },
        ] {
            deliver_tool(state, session.clone(), PartiturTool::Superpowers(tool));
        }
        deliver(state, turn_ended(session));
    }

    /// Takes `session`, which waits at its design, through the plan to task 4 of 11.
    fn executes_task_four(state: &mut State, session: &SessionId) {
        let tool = |state: &mut State, event| {
            deliver_tool(state, session.clone(), PartiturTool::Superpowers(event));
        };
        state.app.approve_gate(session.clone()).unwrap();
        tool(state, SuperpowersEvent::StartWritingPlan);
        tool(
            state,
            SuperpowersEvent::RequestPlanApproval {
                path: "docs/plan.md".into(),
                tasks: (1..=11)
                    .map(|n| PlannedTask {
                        n,
                        title: format!("Task {n}"),
                        files: Vec::new(),
                    })
                    .collect(),
                checkpoints: Vec::new(),
            },
        );
        deliver(state, turn_ended(session));
        state.app.approve_gate(session.clone()).unwrap();
        tool(
            state,
            SuperpowersEvent::StartExecution {
                mode: ExecutionMode::Subagent,
            },
        );
        for n in 1..=3 {
            tool(state, SuperpowersEvent::StartTask { n, note: None });
            tool(state, SuperpowersEvent::FinishTask { n, note: None });
        }
        tool(state, SuperpowersEvent::StartTask { n: 4, note: None });
    }

    #[test]
    fn the_step_column_shows_each_sessions_step() {
        let dir = project_dir();
        let (mut state, backend, [plain, gated]) = state_with_project(&dir);
        waits_at_the_design(&mut state, &backend, &gated);
        let third = state
            .app
            .new_session(state.app.tree().projects[0].id.clone(), Scope::Project)
            .unwrap();
        waits_at_the_design(&mut state, &backend, &third);
        executes_task_four(&mut state, &third);
        let mut ui = simulator(view(&state));

        let gate_row = bounds_of(&mut ui, format!("session-{gated}"));
        let execute_row = bounds_of(&mut ui, format!("session-{third}"));
        let plain_row = bounds_of(&mut ui, format!("session-{plain}"));

        assert!(shows_within(&mut ui, "design · gate", gate_row));
        assert!(shows_within(&mut ui, "execute · 4/11", execute_row));
        assert!(!shows_within(&mut ui, "no workflow", plain_row));
        assert_eq!(shown(&mut ui, "no workflow"), 0);
    }

    #[test]
    fn a_waiting_gate_counts_as_gate_waiting_not_asks_you() {
        let dir = project_dir();
        let (mut state, backend, [asking, gated]) = state_with_project(&dir);
        waits_at_the_design(&mut state, &backend, &gated);
        attach(&mut state, &backend, &asking, "b-asking");
        deliver(
            &mut state,
            BackendEvent::Update {
                session: asking,
                update: asks_permission(),
            },
        );
        {
            let mut ui = simulator(view(&state));
            ui.find("1 gate waiting").unwrap();
            ui.find("1 asks you").unwrap();
        }

        let other = TempDir::new().unwrap();
        let project = state.app.open_project(other.path()).unwrap();
        let second = state.app.new_session(project, Scope::Project).unwrap();
        waits_at_the_design(&mut state, &backend, &second);
        let mut ui = simulator(view(&state));
        ui.find("2 gates waiting").unwrap();
        ui.find("1 asks you").unwrap();
    }

    #[test]
    fn a_status_line_without_gates_names_none() {
        let dir = project_dir();
        let (state, _backend, _sessions) = state_with_project(&dir);
        let mut ui = simulator(view(&state));

        assert!(ui.find("1 gate waiting").is_err());
        assert!(ui.find("0 gates waiting").is_err());
    }

    #[test]
    fn g_opens_the_oldest_waiting_gate_on_the_spec_tab() {
        let (mut state, backend) = empty_state();
        let (older_dir, newer_dir) = (project_dir(), project_dir());
        let older_project = state.app.open_project(older_dir.path()).unwrap();
        let older = state
            .app
            .new_session(older_project, Scope::Project)
            .unwrap();
        let newer_project = state.app.open_project(newer_dir.path()).unwrap();
        let newer = state
            .app
            .new_session(newer_project, Scope::Project)
            .unwrap();
        let press_g = |state: &mut State| {
            let _task = update(state, Message::Key(BareKey::G));
        };

        // Without a gate nothing changes.
        state.tab = Tab::Projects;
        press_g(&mut state);
        assert_eq!(state.tab, Tab::Projects);
        assert_eq!(state.app.current_session(), Some(newer.clone()));

        waits_at_the_design(&mut state, &backend, &older);
        waits_at_the_design(&mut state, &backend, &newer);
        state.app.select_session(newer).unwrap();
        state.tab = Tab::Env;
        press_g(&mut state);

        assert_eq!(state.app.current_session(), Some(older.clone()));
        assert_eq!(state.projects.cursor, Some(older));
        assert_eq!(state.tab, Tab::Spec);
        assert_eq!(state.error, None);
    }

    #[test]
    fn the_key_hints_name_g() {
        let (state, _backend) = empty_state();
        let mut ui = simulator(view(&state));

        ui.find("next gate").unwrap();
    }

    #[test]
    fn the_status_line_names_the_keys() {
        let (state, _backend) = empty_state();
        let mut ui = simulator(view(&state));

        for (keys, action) in KEY_HINTS {
            ui.find(keys).unwrap();
            ui.find(action).unwrap();
        }
    }

    #[test]
    fn closing_a_project_removes_it_from_the_tree() {
        let dir = project_dir();
        let (mut state, _backend, _) = state_with_project(&dir);
        let project = state.app.tree().projects[0].id.clone();
        apply(&mut state, [ProjectsMessage::HoverProject(project.clone())]);

        let messages = click(&state, format!("close-{project}"));
        assert_eq!(
            messages,
            [crate::app::Message::Projects(ProjectsMessage::Close(
                project
            ))]
        );
        for message in messages {
            let _task = update(&mut state, message);
        }

        assert!(state.app.tree().projects.is_empty());
        assert_eq!(state.projects.hovered_project, None);
        let mut ui = simulator(view(&state));
        ui.find("No projects open").unwrap();
    }

    #[test]
    fn closing_a_project_forgets_its_title_field_and_its_cursor() {
        let dir = project_dir();
        let (mut fixture, other) = orchestrated_and_another_project(&dir);
        let projects: Vec<ProjectId> = fixture
            .state
            .app
            .tree()
            .projects
            .iter()
            .map(|project| project.id.clone())
            .collect();
        fixture.state.projects.cursor = Some(other.clone());
        apply(
            &mut fixture.state,
            [ProjectsMessage::StartRename(fixture.orchestrator.clone())],
        );

        // Closing the first project keeps what belongs to the second.
        apply(
            &mut fixture.state,
            [ProjectsMessage::Close(projects[0].clone())],
        );
        assert_eq!(fixture.state.projects.renaming, None);
        assert_eq!(fixture.state.projects.cursor, Some(other.clone()));

        apply(&mut fixture.state, [ProjectsMessage::StartRename(other)]);
        apply(
            &mut fixture.state,
            [ProjectsMessage::Close(projects[1].clone())],
        );
        assert_eq!(fixture.state.projects.renaming, None);
        assert_eq!(fixture.state.projects.cursor, None);
    }

    #[test]
    fn closing_a_project_forgets_what_was_expanded_in_its_sessions() {
        let dir = project_dir();
        let (mut fixture, other) = orchestrated_and_another_project(&dir);
        let first = fixture.state.app.tree().projects[0].id.clone();
        let entry = |session: &SessionId| (session.clone(), 0);
        for session in [&fixture.orchestrator, &fixture.child, &other] {
            fixture.state.session.expanded.insert(entry(session));
            fixture.state.session.expanded_tools.insert(entry(session));
            fixture
                .state
                .session
                .heights
                .record(session, RowKey::Pending, 40.0);
        }

        apply(&mut fixture.state, [ProjectsMessage::Close(first)]);

        // What belongs to the other project's session stays.
        let kept = HashSet::from([entry(&other)]);
        assert_eq!(fixture.state.session.expanded, kept);
        assert_eq!(fixture.state.session.expanded_tools, kept);
        let height = |session: &SessionId| {
            let heights = &fixture.state.session.heights;
            heights.get(session, RowKey::Pending).map(f32::to_bits)
        };
        assert_eq!(height(&fixture.orchestrator), None);
        assert_eq!(height(&fixture.child), None);
        assert_eq!(height(&other), Some(40.0_f32.to_bits()));
    }

    #[test]
    fn a_picked_folder_that_cannot_be_opened_is_reported() {
        let dir = project_dir();
        let (mut state, _backend, _) = state_with_project(&dir);
        let missing = dir.path().join("does-not-exist");

        apply(&mut state, [ProjectsMessage::FolderPicked(Some(missing))]);

        assert!(state.error.is_some());
        assert_eq!(state.app.tree().projects.len(), 1);
    }

    /// Two sessions of a project in `dir` saved in `store`: a top-level one and one spawned
    /// from it, as `(parent, child)`.
    fn stored_parent_and_child(dir: &TempDir, store: &MemoryStore) -> (SessionId, SessionId) {
        let (backends, default) = single_claude();
        let mut app = App::load(
            Box::new(RecordingBackend::new()),
            Box::new(store.clone()),
            Box::new(MemoryGit::new()),
            backends,
            default,
            GlobalSettings::default(),
        )
        .unwrap();
        let project = app.open_project(dir.path()).unwrap();
        let parent = app.new_session(project.clone(), Scope::Project).unwrap();
        let child = app.new_session(project.clone(), Scope::Project).unwrap();
        let mut sessions = store.load_sessions(project.clone()).unwrap();
        for session in &mut sessions {
            if session.id == child {
                session.parent = Some(parent.clone());
            }
        }
        store.save_sessions(project, &sessions).unwrap();
        (parent, child)
    }

    /// The state after a restart with the projects and sessions `store` holds.
    fn restarted(store: MemoryStore) -> State {
        let (backends, default) = single_claude();
        let app = App::load(
            Box::new(RecordingBackend::new()),
            Box::new(store),
            Box::new(MemoryGit::new()),
            backends,
            default,
            GlobalSettings::default(),
        )
        .unwrap();
        boot(app, mpsc::unbounded().1, mpsc::unbounded().1).0
    }

    #[test]
    fn a_project_restored_at_startup_lists_its_sessions() {
        let dir = project_dir();
        let store = MemoryStore::new();
        let (parent, child) = stored_parent_and_child(&dir, &store);

        let mut state = restarted(store);
        state.app.toggle_children(parent.clone()).unwrap();
        let mut ui = simulator(view(&state));

        ui.find(Id::from(format!("session-{parent}"))).unwrap();
        ui.find(Id::from(format!("session-{child}"))).unwrap();
        ui.find("new session").unwrap();
    }

    #[test]
    fn a_spawned_session_is_indented_under_its_parent() {
        let dir = project_dir();
        let store = MemoryStore::new();
        let (parent, child) = stored_parent_and_child(&dir, &store);

        let mut state = restarted(store);
        state.app.toggle_children(parent.clone()).unwrap();
        let mut ui = simulator(view(&state));

        let parent = bounds_of(&mut ui, format!("session-{parent}"));
        let child = bounds_of(&mut ui, format!("session-{child}"));
        assert!((child.x - parent.x - LEVEL_INDENT).abs() < 0.5);
        assert!((child.y - parent.y - ROW_HEIGHT - ROW_GAP).abs() < 0.5);
    }

    #[test]
    fn a_cancelled_folder_dialog_changes_nothing() {
        let dir = project_dir();
        let (mut state, _backend, _) = state_with_project(&dir);

        apply(&mut state, [ProjectsMessage::FolderPicked(None)]);

        assert_eq!(state.error, None);
        assert_eq!(state.tab, Tab::Projects);
        assert_eq!(state.app.tree().projects.len(), 1);
    }
    /// The project of a state from [`state_with_project`] and the first of its two sessions,
    /// archived.
    fn one_archived(state: &mut State, session: &SessionId) -> ProjectId {
        state.app.archive(session.clone()).unwrap();
        state.app.tree().projects[0].id.clone()
    }

    /// The date the archived `session` of the first project shows.
    fn archived_on(state: &State, session: &SessionId) -> String {
        let tree = state.app.tree();
        let node = tree.projects[0]
            .archived
            .iter()
            .find(|node| &node.id == session)
            .unwrap();
        partitur_core::day_month(node.archived.unwrap().at)
    }

    #[test]
    fn the_archived_group_is_hidden_without_archived_sessions() {
        let dir = project_dir();
        let (state, _backend, _) = state_with_project(&dir);
        let project = state.app.tree().projects[0].id.clone();
        let mut ui = simulator(view(&state));

        assert!(ui.find(Id::from(format!("archived-{project}"))).is_err());
        assert!(ui.find("archived · 0").is_err());
    }

    #[test]
    fn the_archived_group_starts_collapsed_and_counts_its_sessions() {
        let dir = project_dir();
        let (mut state, _backend, [first, second]) = state_with_project(&dir);
        let project = one_archived(&mut state, &first);
        let mut ui = simulator(view(&state));

        ui.find(Id::from(format!("archived-{project}"))).unwrap();
        ui.find("archived · 1").unwrap();
        ui.find(Id::from(format!("session-{second}"))).unwrap();
        assert!(ui.find(Id::from(format!("session-{first}"))).is_err());
    }

    #[test]
    fn the_archived_group_sits_below_the_new_session_row() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        let project = one_archived(&mut state, &first);
        let mut ui = simulator(view(&state));

        let new_session = bounds_of(&mut ui, format!("new-session-{project}"));
        let group = bounds_of(&mut ui, format!("archived-{project}"));

        assert!(group.y > new_session.y + ROW_HEIGHT);
    }

    #[test]
    fn clicking_the_archived_group_shows_its_sessions_with_their_date() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        let project = one_archived(&mut state, &first);

        let messages = click(&state, format!("archived-{project}"));
        assert_eq!(
            messages,
            [crate::app::Message::Projects(
                ProjectsMessage::ToggleArchived(project.clone())
            )]
        );
        apply(
            &mut state,
            [ProjectsMessage::ToggleArchived(project.clone())],
        );

        let date = format!("archived {}", archived_on(&state, &first));
        {
            let mut ui = simulator(view(&state));
            let row = bounds_of(&mut ui, format!("session-{first}"));
            assert!(shows_within(&mut ui, &date, row));
            // Its buttons wait for the pointer.
            assert!(ui.find(Id::from(format!("restore-{first}"))).is_err());
            let group = bounds_of(&mut ui, format!("archived-{project}"));
            assert!((row.x - group.x - LEVEL_INDENT).abs() < 0.5);
        }

        apply(&mut state, [ProjectsMessage::ToggleArchived(project)]);
        let mut ui = simulator(view(&state));
        assert!(ui.find(Id::from(format!("session-{first}"))).is_err());
    }

    #[test]
    fn an_archived_orchestrator_shows_its_children_one_level_deeper() {
        let mut fixture = orchestrated();
        expand(&mut fixture);
        let orchestrator = fixture.orchestrator.clone();
        let child = fixture.child.clone();
        fixture.state.app.archive(orchestrator.clone()).unwrap();
        let project = fixture.state.app.tree().projects[0].id.clone();
        apply(
            &mut fixture.state,
            [ProjectsMessage::ToggleArchived(project)],
        );
        let mut ui = simulator(view(&fixture.state));

        let parent = bounds_of(&mut ui, format!("session-{orchestrator}"));
        let spawned = bounds_of(&mut ui, format!("session-{child}"));
        assert!((spawned.x - parent.x - LEVEL_INDENT).abs() < 0.5);
        assert!(ui.find("1 child · api running").is_err());
    }

    #[test]
    fn a_folded_archived_orchestrator_sums_up_its_children() {
        let mut fixture = orchestrated();
        let orchestrator = fixture.orchestrator.clone();
        fixture.state.app.archive(orchestrator.clone()).unwrap();
        let project = fixture.state.app.tree().projects[0].id.clone();
        apply(
            &mut fixture.state,
            [ProjectsMessage::ToggleArchived(project)],
        );

        let mut ui = simulator(view(&fixture.state));
        assert!(
            ui.find(Id::from(format!("session-{}", fixture.child)))
                .is_err()
        );
        let summary = bounds_of(&mut ui, format!("summary-{orchestrator}"));
        let text = text_within(&mut ui, "1 child · api running", summary).unwrap();
        // The text starts where the title of an archived child row would.
        let parent = bounds_of(&mut ui, format!("session-{orchestrator}"));
        let title = text_within(&mut ui, "untitled", parent).unwrap();
        assert!((text.x - title.x - LEVEL_INDENT).abs() < 0.5);
    }

    #[test]
    fn restore_puts_the_session_back_in_the_live_list() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        let project = one_archived(&mut state, &first);
        apply(
            &mut state,
            [ProjectsMessage::ToggleArchived(project.clone())],
        );
        state.projects.hovered = Some(first.clone());

        let messages = click(&state, format!("restore-{first}"));
        assert_eq!(
            messages,
            [crate::app::Message::Projects(ProjectsMessage::Restore(
                first.clone()
            ))]
        );
        apply(&mut state, [ProjectsMessage::Restore(first.clone())]);

        assert_eq!(state.error, None);
        // The pointer stayed where the archived row was, not on the live row.
        assert_eq!(state.projects.hovered, None);
        assert_eq!(state.app.tree().projects[0].sessions.len(), 2);
        let mut ui = simulator(view(&state));
        ui.find(Id::from(format!("session-{first}"))).unwrap();
        assert!(ui.find(Id::from(format!("archived-{project}"))).is_err());
    }

    #[test]
    fn clicking_an_archived_row_opens_it_in_the_session_tab() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        let project = one_archived(&mut state, &first);
        apply(&mut state, [ProjectsMessage::ToggleArchived(project)]);

        let messages = click(&state, format!("session-{first}"));
        assert_eq!(
            messages,
            [crate::app::Message::Projects(ProjectsMessage::Select(
                first.clone()
            ))]
        );
        apply(&mut state, [ProjectsMessage::Select(first.clone())]);

        assert_eq!(state.tab, Tab::Session);
        assert_eq!(state.app.current_session(), Some(first.clone()));
        assert_eq!(state.projects.archived_open.len(), 1);
    }

    #[test]
    fn selecting_an_archived_session_opens_its_group() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        let project = one_archived(&mut state, &first);

        apply(&mut state, [ProjectsMessage::Select(first.clone())]);

        assert!(state.projects.archived_open.contains(&project));
        assert_eq!(state.projects.cursor, Some(first));
    }

    #[test]
    fn the_status_line_counts_archived_sessions_apart() {
        let dir = project_dir();
        let (mut state, backend, [first, second]) = state_with_project(&dir);
        for session in [&first, &second] {
            attach(&mut state, &backend, session, "b");
            state.app.select_session(session.clone()).unwrap();
            state.app.submit_prompt("go", Vec::new()).unwrap();
            deliver(&mut state, turn_ended(session));
        }
        let _ = one_archived(&mut state, &first);
        let mut ui = simulator(view(&state));

        ui.find("1 archived").unwrap();
        ui.find("1 done").unwrap();
        assert!(ui.find("2 done").is_err());
    }

    #[test]
    fn the_status_line_names_no_archived_sessions_when_there_are_none() {
        let dir = project_dir();
        let (state, _backend, _) = state_with_project(&dir);
        let mut ui = simulator(view(&state));

        assert!(ui.find("0 archived").is_err());
    }

    #[test]
    fn arrow_keys_reach_archived_rows_only_while_the_group_is_open() {
        let dir = project_dir();
        let (mut state, _backend, [first, second]) = state_with_project(&dir);
        let project = one_archived(&mut state, &first);
        state.tab = Tab::Projects;
        state.projects.cursor = Some(second.clone());

        apply(&mut state, [ProjectsMessage::CursorDown]);
        assert_eq!(state.projects.cursor, Some(second.clone()));

        apply(
            &mut state,
            [
                ProjectsMessage::ToggleArchived(project.clone()),
                ProjectsMessage::CursorDown,
            ],
        );
        assert_eq!(state.projects.cursor, Some(first.clone()));

        apply(&mut state, [ProjectsMessage::ToggleArchived(project)]);
        assert_eq!(state.projects.cursor, None);
    }

    /// Asks to archive `session` and confirms in the sheet that opens.
    fn archive_confirmed(state: &mut State, session: SessionId) {
        apply(state, [ProjectsMessage::Archive(session)]);
        let _task = update(
            state,
            crate::app::Message::Confirm(crate::confirm_sheet::ConfirmMessage::Confirm),
        );
    }

    /// Presses the bare `key` the way the keyboard subscription reports it.
    fn press_key(state: &mut State, key: crate::app::BareKey) {
        let _task = update(state, crate::app::Message::Key(key));
    }

    #[test]
    fn the_selected_row_shows_the_archive_button() {
        let dir = project_dir();
        let (mut state, _backend, [first, second]) = state_with_project(&dir);
        state.projects.cursor = Some(first.clone());
        let mut ui = simulator(view(&state));

        let row = bounds_of(&mut ui, format!("session-{first}"));
        let strip = bounds_of(&mut ui, format!("archive-{first}"));
        assert!(row.contains(strip.center()));
        // The strip ends the title cell, so it sits clear of the title's start, left of the
        // status, step, context and activity columns.
        assert!(strip.x > row.x + row.width / 3.0);
        assert!(shows_within(&mut ui, "Archive", strip));
        assert!(shows_within(&mut ui, "A", strip));
        assert!(ui.find(Id::from(format!("archive-{second}"))).is_err());
    }

    #[test]
    fn hovering_a_row_shows_its_action_strip() {
        let dir = project_dir();
        let (mut state, _backend, [first, second]) = state_with_project(&dir);
        state.projects.cursor = Some(first.clone());

        apply(&mut state, [ProjectsMessage::Hover(second.clone())]);
        {
            let mut ui = simulator(view(&state));
            ui.find(Id::from(format!("archive-{second}"))).unwrap();
            ui.find(Id::from(format!("archive-{first}"))).unwrap();
        }

        // Leaving a row the pointer already left for another changes nothing.
        apply(&mut state, [ProjectsMessage::Unhover(first.clone())]);
        assert_eq!(state.projects.hovered, Some(second.clone()));
        apply(&mut state, [ProjectsMessage::Unhover(second.clone())]);
        assert_eq!(state.projects.hovered, None);
        let mut ui = simulator(view(&state));
        assert!(ui.find(Id::from(format!("archive-{second}"))).is_err());
    }

    #[test]
    fn moving_the_pointer_onto_a_row_hovers_it() {
        let dir = project_dir();
        let (state, _backend, [first, _]) = state_with_project(&dir);
        let mut ui = simulator(view(&state));
        let row = bounds_of(&mut ui, format!("session-{first}"));

        ui.point_at(row.center());
        ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
            position: row.center(),
        })]);
        let messages: Vec<_> = ui.into_messages().collect();

        assert!(
            messages.contains(&crate::app::Message::Projects(ProjectsMessage::Hover(
                first
            )))
        );
    }

    #[test]
    fn the_archive_button_archives_the_session() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        state.projects.cursor = Some(first.clone());

        let messages = click(&state, format!("archive-{first}"));
        assert_eq!(
            messages,
            [crate::app::Message::Projects(ProjectsMessage::Archive(
                first.clone()
            ))]
        );
        archive_confirmed(&mut state, first.clone());

        assert_eq!(state.error, None);
        assert_eq!(state.app.tree().projects[0].archived.len(), 1);
    }

    #[test]
    fn a_running_session_offers_stop_and_archive() {
        let fixture = orchestrated();
        let mut state = fixture.state;
        state.projects.cursor = Some(fixture.orchestrator.clone());
        let mut ui = simulator(view(&state));

        let strip = bounds_of(&mut ui, format!("archive-{}", fixture.orchestrator));
        assert!(shows_within(&mut ui, "Stop and archive", strip));
    }

    #[test]
    fn a_child_row_cannot_be_archived() {
        let mut fixture = orchestrated();
        expand(&mut fixture);
        fixture.state.projects.cursor = Some(fixture.child.clone());

        let messages = click(&fixture.state, format!("archive-{}", fixture.child));

        // The pressless button lets the click through to its row, which selects the child.
        assert_eq!(
            messages,
            [crate::app::Message::Projects(ProjectsMessage::Select(
                fixture.child.clone()
            ))]
        );
        assert_eq!(fixture.state.app.tree().projects[0].archived.len(), 0);
    }

    #[test]
    fn pressing_a_archives_the_session_at_the_cursor() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        state.tab = Tab::Projects;
        state.projects.cursor = Some(first.clone());

        press_key(&mut state, crate::app::BareKey::A);
        assert!(state.confirm.is_some());
        press_key(&mut state, crate::app::BareKey::Enter);

        let tree = state.app.tree();
        assert_eq!(tree.projects[0].archived.len(), 1);
        assert_eq!(tree.projects[0].archived[0].id, first);
    }

    #[test]
    fn pressing_a_on_a_child_or_without_a_cursor_does_nothing() {
        let mut fixture = orchestrated();
        fixture.state.tab = Tab::Projects;
        fixture.state.projects.cursor = Some(fixture.child.clone());

        press_key(&mut fixture.state, crate::app::BareKey::A);
        fixture.state.projects.cursor = None;
        press_key(&mut fixture.state, crate::app::BareKey::A);

        assert_eq!(fixture.state.confirm, None);
        assert_eq!(fixture.state.error, None);
        assert_eq!(fixture.state.app.tree().projects[0].archived.len(), 0);
    }

    #[test]
    fn pressing_a_on_another_tab_does_nothing() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        state.tab = Tab::Session;
        state.projects.cursor = Some(first);

        press_key(&mut state, crate::app::BareKey::A);

        assert_eq!(state.confirm, None);
        assert_eq!(state.app.tree().projects[0].archived.len(), 0);
    }

    #[test]
    fn archiving_moves_the_cursor_off_the_archived_rows() {
        let dir = TempDir::new().unwrap();
        let (mut state, _backend, [first, second]) = state_with_project(&dir);
        let project = state.app.tree().projects[0].id.clone();
        let third = state.app.new_session(project, Scope::Project).unwrap();
        // Top to bottom: third, second, first.
        state.projects.cursor = Some(second.clone());

        archive_confirmed(&mut state, second);
        assert_eq!(state.projects.cursor, Some(first.clone()));

        archive_confirmed(&mut state, first);
        assert_eq!(state.projects.cursor, Some(third.clone()));

        archive_confirmed(&mut state, third);
        assert_eq!(state.projects.cursor, None);
    }

    #[test]
    fn archiving_keeps_a_cursor_the_open_group_still_shows() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        let project = state.app.tree().projects[0].id.clone();
        state.projects.archived_open.insert(project);
        state.projects.cursor = Some(first.clone());

        archive_confirmed(&mut state, first.clone());

        assert_eq!(state.projects.cursor, Some(first));
    }

    #[test]
    fn the_status_line_names_the_archive_key() {
        let (state, _backend) = empty_state();
        let mut ui = simulator(view(&state));

        ui.find("archive").unwrap();
    }

    #[test]
    fn the_status_line_counts_idle_and_shows_three_key_hints() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        let _ = one_archived(&mut state, &first);
        let mut ui = simulator(view(&state));

        for shown in [
            "1 idle",
            "1 archived",
            "Enter",
            "open",
            "N",
            "new session",
            "A",
        ] {
            ui.find(shown).unwrap();
        }
        ui.find("archive").unwrap();
        for absent in ["delete", "D", "Tab", "0 done", "0 running", "0 asks you"] {
            assert!(ui.find(absent).is_err(), "{absent}");
        }
    }

    #[test]
    fn the_status_line_joins_its_counts_with_dots() {
        let dir = project_dir();
        let (mut state, backend, [first, second]) = state_with_project(&dir);
        attach(&mut state, &backend, &first, "b");
        state.app.select_session(first.clone()).unwrap();
        state.app.submit_prompt("go", Vec::new()).unwrap();
        deliver(&mut state, turn_ended(&first));
        let mut ui = simulator(view(&state));
        let idle = ui.find("1 idle").unwrap().bounds();
        let done = ui.find("1 done").unwrap().bounds();
        let dot = ui
            .find(move |candidate: Candidate<'_>| match candidate {
                Candidate::Text {
                    content, bounds, ..
                } if content == "·" && bounds.x > idle.x && bounds.x < done.x => Some(()),
                _ => None,
            })
            .is_ok();
        assert!(dot);
        drop(ui);

        attach(&mut state, &backend, &second, "c");
        deliver(
            &mut state,
            BackendEvent::Update {
                session: second.clone(),
                update: Update::Failed("the agent crashed".into()),
            },
        );
        let mut ui = simulator(view(&state));
        assert!(ui.find("1 idle").is_err());
        let done = ui.find("1 done").unwrap().bounds();
        let failed = ui.find("1 failed").unwrap().bounds();
        assert!(done.x < failed.x);
    }

    /// Asks to delete `session` and confirms in the sheet that opens.
    fn delete_confirmed(state: &mut State, session: SessionId) {
        apply(state, [ProjectsMessage::Delete(session)]);
        let _task = update(
            state,
            crate::app::Message::Confirm(crate::confirm_sheet::ConfirmMessage::Confirm),
        );
    }

    #[test]
    fn the_selected_row_shows_the_delete_button_after_the_archive_button() {
        let dir = project_dir();
        let (mut state, _backend, [first, second]) = state_with_project(&dir);
        state.projects.cursor = Some(first.clone());
        let mut ui = simulator(view(&state));

        let row = bounds_of(&mut ui, format!("session-{first}"));
        let archive = bounds_of(&mut ui, format!("archive-{first}"));
        let delete = bounds_of(&mut ui, format!("delete-{first}"));
        assert!(row.contains(delete.center()));
        assert!(delete.x > archive.x + archive.width);
        assert!(shows_within(&mut ui, "Delete", delete));
        assert!(shows_within(&mut ui, "D", delete));
        assert!(ui.find(Id::from(format!("delete-{second}"))).is_err());
    }

    #[test]
    fn the_delete_button_deletes_the_session() {
        let dir = project_dir();
        let (mut state, _backend, [first, second]) = state_with_project(&dir);
        state.projects.cursor = Some(first.clone());

        let messages = click(&state, format!("delete-{first}"));
        assert_eq!(
            messages,
            [crate::app::Message::Projects(ProjectsMessage::Delete(
                first.clone()
            ))]
        );
        apply(&mut state, [ProjectsMessage::Delete(first.clone())]);
        assert_eq!(
            state.confirm.as_ref().map(|sheet| sheet.action),
            Some(Action::Delete)
        );
        let _task = update(
            &mut state,
            crate::app::Message::Confirm(crate::confirm_sheet::ConfirmMessage::Confirm),
        );

        assert_eq!(state.error, None);
        let tree = state.app.tree();
        assert_eq!(tree.projects[0].sessions.len(), 1);
        assert_eq!(tree.projects[0].sessions[0].id, second);
        assert!(tree.projects[0].archived.is_empty());
    }

    #[test]
    fn a_running_session_offers_stop_and_delete() {
        let fixture = orchestrated();
        let mut state = fixture.state;
        state.projects.cursor = Some(fixture.orchestrator.clone());
        let mut ui = simulator(view(&state));

        let delete = bounds_of(&mut ui, format!("delete-{}", fixture.orchestrator));
        assert!(shows_within(&mut ui, "Stop and delete", delete));
    }

    #[test]
    fn a_child_row_cannot_be_deleted() {
        let mut fixture = orchestrated();
        expand(&mut fixture);
        fixture.state.projects.cursor = Some(fixture.child.clone());

        let messages = click(&fixture.state, format!("delete-{}", fixture.child));
        // The pressless button lets the click through to its row, which selects the child.
        assert_eq!(
            messages,
            [crate::app::Message::Projects(ProjectsMessage::Select(
                fixture.child.clone()
            ))]
        );
        assert_eq!(fixture.state.app.tree().projects[0].sessions.len(), 1);
    }

    #[test]
    fn a_hovered_archived_row_shows_restore_and_delete_buttons() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        let project = one_archived(&mut state, &first);
        apply(&mut state, [ProjectsMessage::ToggleArchived(project)]);
        apply(&mut state, [ProjectsMessage::Hover(first.clone())]);
        {
            let mut ui = simulator(view(&state));
            let row = bounds_of(&mut ui, format!("session-{first}"));
            let restore = bounds_of(&mut ui, format!("restore-{first}"));
            let delete = bounds_of(&mut ui, format!("delete-{first}"));
            assert!(row.contains(restore.center()));
            assert!(row.contains(delete.center()));
            assert!(delete.x > restore.x + restore.width);
            assert!(shows_within(&mut ui, "Restore", restore));
            assert!(shows_within(&mut ui, "Delete", delete));
            assert!(shows_within(&mut ui, "D", delete));
        }

        let messages = click(&state, format!("delete-{first}"));
        assert_eq!(
            messages,
            [crate::app::Message::Projects(ProjectsMessage::Delete(
                first.clone()
            ))]
        );
        delete_confirmed(&mut state, first);

        assert_eq!(state.error, None);
        assert!(state.app.tree().projects[0].archived.is_empty());
    }

    #[test]
    fn a_hovered_archived_child_shows_no_buttons() {
        let mut fixture = orchestrated();
        let (orchestrator, child) = (fixture.orchestrator.clone(), fixture.child.clone());
        fixture.state.app.archive(orchestrator.clone()).unwrap();
        let project = fixture.state.app.tree().projects[0].id.clone();
        apply(
            &mut fixture.state,
            [ProjectsMessage::ToggleArchived(project)],
        );

        fixture.state.projects.hovered = Some(orchestrator.clone());
        {
            let mut ui = simulator(view(&fixture.state));
            ui.find(Id::from(format!("restore-{orchestrator}")))
                .unwrap();
            ui.find(Id::from(format!("delete-{orchestrator}"))).unwrap();
        }

        // The child goes with its orchestrator, so its row offers neither.
        fixture.state.projects.hovered = Some(child.clone());
        let mut ui = simulator(view(&fixture.state));
        assert!(ui.find(Id::from(format!("restore-{child}"))).is_err());
        assert!(ui.find(Id::from(format!("delete-{child}"))).is_err());
    }

    #[test]
    fn a_highlighted_archived_row_shows_no_buttons_without_the_pointer() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        let project = one_archived(&mut state, &first);
        apply(&mut state, [ProjectsMessage::ToggleArchived(project)]);
        state.projects.cursor = Some(first.clone());
        {
            let mut ui = simulator(view(&state));
            assert!(ui.find(Id::from(format!("restore-{first}"))).is_err());
            assert!(ui.find(Id::from(format!("delete-{first}"))).is_err());
        }

        apply(&mut state, [ProjectsMessage::Hover(first.clone())]);
        apply(&mut state, [ProjectsMessage::Unhover(first.clone())]);
        let mut ui = simulator(view(&state));
        assert!(ui.find(Id::from(format!("restore-{first}"))).is_err());
    }

    #[test]
    fn moving_the_pointer_onto_an_archived_row_hovers_it() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        let project = one_archived(&mut state, &first);
        apply(&mut state, [ProjectsMessage::ToggleArchived(project)]);
        let mut ui = simulator(view(&state));
        let row = bounds_of(&mut ui, format!("session-{first}"));

        ui.point_at(row.center());
        ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
            position: row.center(),
        })]);
        let messages: Vec<_> = ui.into_messages().collect();

        assert!(
            messages.contains(&crate::app::Message::Projects(ProjectsMessage::Hover(
                first
            )))
        );
    }

    #[test]
    fn pressing_d_deletes_the_session_at_the_cursor() {
        let dir = project_dir();
        let (mut state, _backend, [first, second]) = state_with_project(&dir);
        state.tab = Tab::Projects;
        state.projects.cursor = Some(first.clone());

        press_key(&mut state, crate::app::BareKey::D);
        assert_eq!(
            state.confirm.as_ref().map(|sheet| sheet.action),
            Some(Action::Delete)
        );
        press_key(&mut state, crate::app::BareKey::Enter);

        let tree = state.app.tree();
        assert_eq!(tree.projects[0].sessions.len(), 1);
        assert_eq!(tree.projects[0].sessions[0].id, second);
    }

    #[test]
    fn pressing_d_deletes_an_archived_session_at_the_cursor() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        let project = one_archived(&mut state, &first);
        apply(&mut state, [ProjectsMessage::ToggleArchived(project)]);
        state.tab = Tab::Projects;
        state.projects.cursor = Some(first);

        press_key(&mut state, crate::app::BareKey::D);
        press_key(&mut state, crate::app::BareKey::Enter);

        assert_eq!(state.error, None);
        assert!(state.app.tree().projects[0].archived.is_empty());
    }

    #[test]
    fn pressing_d_on_a_child_or_without_a_cursor_does_nothing() {
        let mut fixture = orchestrated();
        fixture.state.tab = Tab::Projects;
        fixture.state.projects.cursor = Some(fixture.child.clone());

        press_key(&mut fixture.state, crate::app::BareKey::D);
        fixture.state.projects.cursor = None;
        press_key(&mut fixture.state, crate::app::BareKey::D);

        assert_eq!(fixture.state.confirm, None);
        assert_eq!(fixture.state.error, None);
        assert_eq!(fixture.state.app.tree().projects[0].sessions.len(), 1);
    }

    #[test]
    fn pressing_d_on_another_tab_does_nothing() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        state.tab = Tab::Session;
        state.projects.cursor = Some(first);

        press_key(&mut state, crate::app::BareKey::D);

        assert_eq!(state.confirm, None);
        assert_eq!(state.app.tree().projects[0].sessions.len(), 2);
    }

    #[test]
    fn deleting_moves_the_cursor_to_the_nearest_row_still_shown() {
        let dir = TempDir::new().unwrap();
        let (mut state, _backend, [first, second]) = state_with_project(&dir);
        let project = state.app.tree().projects[0].id.clone();
        let third = state.app.new_session(project, Scope::Project).unwrap();
        // Top to bottom: third, second, first.
        state.projects.cursor = Some(second.clone());

        delete_confirmed(&mut state, second);
        assert_eq!(state.projects.cursor, Some(first.clone()));

        delete_confirmed(&mut state, first);
        assert_eq!(state.projects.cursor, Some(third.clone()));

        delete_confirmed(&mut state, third);
        assert_eq!(state.projects.cursor, None);
    }

    #[test]
    fn deleting_keeps_a_cursor_on_another_row() {
        let dir = project_dir();
        let (mut state, _backend, [first, second]) = state_with_project(&dir);
        state.projects.cursor = Some(second.clone());

        delete_confirmed(&mut state, first);

        assert_eq!(state.projects.cursor, Some(second));
    }

    #[test]
    fn deleting_the_open_session_shows_the_projects_tab() {
        for tab in [Tab::Session, Tab::Env] {
            let dir = project_dir();
            let (mut state, _backend, [first, _]) = state_with_project(&dir);
            apply(&mut state, [ProjectsMessage::Select(first.clone())]);
            state.tab = tab;

            delete_confirmed(&mut state, first);

            assert_eq!(state.app.current_session(), None);
            assert_eq!(state.tab, Tab::Projects);
        }
    }

    #[test]
    fn deleting_an_orchestrator_whose_child_is_open_shows_the_projects_tab() {
        let mut fixture = orchestrated();
        apply(
            &mut fixture.state,
            [ProjectsMessage::Select(fixture.child.clone())],
        );
        assert_eq!(fixture.state.tab, Tab::Session);

        delete_confirmed(&mut fixture.state, fixture.orchestrator.clone());

        assert_eq!(fixture.state.app.current_session(), None);
        assert_eq!(fixture.state.tab, Tab::Projects);
        assert!(fixture.state.app.tree().projects[0].sessions.is_empty());
    }

    #[test]
    fn deleting_another_session_keeps_the_open_tab() {
        let dir = project_dir();
        let (mut state, _backend, [first, second]) = state_with_project(&dir);
        apply(&mut state, [ProjectsMessage::Select(second.clone())]);

        delete_confirmed(&mut state, first);

        assert_eq!(state.app.current_session(), Some(second));
        assert_eq!(state.tab, Tab::Session);
    }

    #[test]
    fn deleting_forgets_what_was_remembered_about_the_family() {
        let dir = project_dir();
        let (mut fixture, other) = orchestrated_and_another_project(&dir);
        let entry = |session: &SessionId| (session.clone(), 0);
        for session in [&fixture.orchestrator, &fixture.child, &other] {
            fixture.state.session.expanded.insert(entry(session));
            fixture.state.session.expanded_tools.insert(entry(session));
        }
        fixture.state.projects.hovered = Some(fixture.child.clone());
        fixture.state.projects.renaming = Some((fixture.child.clone(), "typed".into()));

        delete_confirmed(&mut fixture.state, fixture.orchestrator.clone());

        assert_eq!(fixture.state.error, None);
        let kept = HashSet::from([entry(&other)]);
        assert_eq!(fixture.state.session.expanded, kept);
        assert_eq!(fixture.state.session.expanded_tools, kept);
        assert_eq!(fixture.state.projects.renaming, None);
        assert_eq!(fixture.state.projects.hovered, None);
    }

    #[test]
    fn deleting_keeps_a_hovered_row_of_another_session() {
        let dir = project_dir();
        let (mut state, _backend, [first, second]) = state_with_project(&dir);
        state.projects.hovered = Some(second.clone());

        delete_confirmed(&mut state, first);

        assert_eq!(state.projects.hovered, Some(second));
    }

    #[test]
    fn a_refused_delete_keeps_the_rows_the_cursor_and_the_tab() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        apply(&mut state, [ProjectsMessage::Select(first.clone())]);
        state.projects.hovered = Some(first.clone());

        assert!(!delete(&mut state, SessionId::new(), &[]));

        assert!(state.error.is_some());
        assert_eq!(state.tab, Tab::Session);
        assert_eq!(state.projects.cursor, Some(first.clone()));
        assert_eq!(state.projects.hovered, Some(first));
        assert_eq!(state.app.tree().projects[0].sessions.len(), 2);
    }

    /// Where the text `label` is shown inside `area`, if it is.
    fn text_within(
        ui: &mut Simulator<'_, crate::app::Message>,
        label: &str,
        area: Rectangle,
    ) -> Option<Rectangle> {
        ui.find(move |candidate: Candidate<'_>| match candidate {
            Candidate::Text {
                content, bounds, ..
            } if content == label && area.contains(bounds.center()) => Some(bounds),
            _ => None,
        })
        .ok()
    }

    /// The state of [`state_with_project`] whose newer session is attached to its backend
    /// session and runs the turn `prompt` started, with that session.
    fn running(dir: &TempDir, prompt: &str) -> (State, SessionId) {
        let (mut state, backend, [_, second]) = state_with_project(dir);
        attach(&mut state, &backend, &second, "b-second");
        state.app.submit_prompt(prompt, Vec::new()).unwrap();
        (state, second)
    }

    /// Feeds `update` for `session` through the application, as its backend would.
    fn report(state: &mut State, session: &SessionId, update: Update) {
        deliver(
            state,
            BackendEvent::Update {
                session: session.clone(),
                update,
            },
        );
    }

    /// The node of `session` in the tree of `state`, at any depth.
    fn node(state: &State, session: &SessionId) -> SessionNode {
        fn find(sessions: &[SessionNode], session: &SessionId) -> Option<SessionNode> {
            sessions.iter().find_map(|node| {
                (&node.id == session)
                    .then(|| node.clone())
                    .or_else(|| find(&node.children, session))
            })
        }
        state
            .app
            .tree()
            .projects
            .iter()
            .find_map(|project| find(&project.sessions, session))
            .unwrap()
    }

    #[test]
    fn status_pills_line_up_in_one_column() {
        let dir = project_dir();
        let (mut state, _backend, [first, second]) = state_with_project(&dir);
        state.app.rename_session(first.clone(), "Short").unwrap();
        state
            .app
            .rename_session(second.clone(), "A much longer title than the other one")
            .unwrap();

        let mut ui = simulator(view(&state));
        let first_row = bounds_of(&mut ui, format!("session-{first}"));
        let second_row = bounds_of(&mut ui, format!("session-{second}"));
        let first_pill = text_within(&mut ui, "idle", first_row).unwrap();
        let second_pill = text_within(&mut ui, "idle", second_row).unwrap();
        assert!((first_pill.x - second_pill.x).abs() < 0.5);
    }

    #[test]
    fn a_skill_shows_as_a_chip_before_the_title() {
        let dir = project_dir();
        let (mut state, session) = running(&dir, "/brainstorming Worktree support");
        report(
            &mut state,
            &session,
            Update::Title("Worktree support".into()),
        );

        let mut ui = simulator(view(&state));
        let row = bounds_of(&mut ui, format!("session-{session}"));
        let chip = text_within(&mut ui, "/brainstorming", row).unwrap();
        let title = text_within(&mut ui, "Worktree support", row).unwrap();
        assert!(chip.x < title.x);
        assert!(ui.find("/brainstorming Worktree support").is_err());
    }

    #[test]
    fn the_context_column_shows_only_for_live_sessions() {
        let dir = project_dir();
        let (mut state, session) = running(&dir, "Build it");
        report(
            &mut state,
            &session,
            Update::Usage {
                used: 68_000,
                size: 1_000_000,
            },
        );
        {
            let mut ui = simulator(view(&state));
            let row = bounds_of(&mut ui, format!("session-{session}"));
            assert!(shows_within(&mut ui, "running", row));
            assert!(shows_within(&mut ui, "68k / 1M", row));
        }

        deliver(&mut state, turn_ended(&session));

        let mut ui = simulator(view(&state));
        let row = bounds_of(&mut ui, format!("session-{session}"));
        assert!(shows_within(&mut ui, "done", row));
        assert!(ui.find("68k / 1M").is_err());
    }

    #[test]
    fn the_activity_column_says_how_long_ago() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        state.now = node(&state, &first).active_at + 240;

        let mut ui = simulator(view(&state));
        let row = bounds_of(&mut ui, format!("session-{first}"));
        assert!(shows_within(&mut ui, "4 min", row));
    }

    #[test]
    fn the_columns_sit_in_their_order() {
        let dir = project_dir();
        let (mut state, session) = running(&dir, "Build it");
        report(
            &mut state,
            &session,
            Update::Usage {
                used: 68_000,
                size: 1_000_000,
            },
        );
        state.now = node(&state, &session).active_at;

        let mut ui = simulator(view(&state));
        let row = bounds_of(&mut ui, format!("session-{session}"));
        let title = text_within(&mut ui, "untitled", row).unwrap();
        let status = text_within(&mut ui, "running", row).unwrap();
        let context = text_within(&mut ui, "68k / 1M", row).unwrap();
        let activity = text_within(&mut ui, "now", row).unwrap();
        assert!(title.x < status.x);
        assert!(status.x < context.x);
        assert!(context.x < activity.x);
        // The activity is right-aligned at the row's text edge.
        assert!((row.x + row.width - (activity.x + activity.width)).abs() < 1.0);
    }

    #[test]
    fn an_orchestrator_starts_collapsed_with_a_summary_row() {
        let mut fixture = orchestrated();
        let child_row = Id::from(format!("session-{}", fixture.child));
        {
            let mut ui = simulator(view(&fixture.state));
            assert!(ui.find(child_row.clone()).is_err());
            ui.find("1 child · api running").unwrap();
        }

        let messages = click(&fixture.state, format!("toggle-{}", fixture.orchestrator));
        assert_eq!(
            messages,
            [crate::app::Message::Projects(
                ProjectsMessage::ToggleChildren(fixture.orchestrator.clone())
            )]
        );
        for message in messages {
            let _task = update(&mut fixture.state, message);
        }
        {
            let mut ui = simulator(view(&fixture.state));
            ui.find(child_row).unwrap();
            assert!(ui.find("1 child · api running").is_err());
        }

        let (backends, default) = single_claude();
        let reloaded = App::load(
            Box::new(RecordingBackend::new()),
            Box::new(fixture.store.clone()),
            Box::new(MemoryGit::new()),
            backends,
            default,
            GlobalSettings::default(),
        )
        .unwrap();
        let tree = reloaded.tree();
        assert!(tree.projects[0].sessions[0].expanded);
    }

    #[test]
    fn the_children_summary_counts_and_names_every_child() {
        let fixture = orchestrated();
        let mut orchestrator = node(&fixture.state, &fixture.orchestrator);
        assert_eq!(
            children_summary_text(&orchestrator),
            "1 child · api running"
        );

        let mut second = orchestrator.children[0].clone();
        second.scope_label = "web".into();
        second.status = Status::Idle;
        orchestrator.children.push(second);
        assert_eq!(
            children_summary_text(&orchestrator),
            "2 children · api running · web idle"
        );
    }

    #[test]
    fn the_summary_row_sits_at_the_childs_title() {
        let mut fixture = orchestrated();
        let (summary, text) = {
            let mut ui = simulator(view(&fixture.state));
            let summary = bounds_of(&mut ui, format!("summary-{}", fixture.orchestrator));
            let text = text_within(&mut ui, "1 child · api running", summary).unwrap();
            (summary, text)
        };
        apply(
            &mut fixture.state,
            [ProjectsMessage::ToggleChildren(
                fixture.orchestrator.clone(),
            )],
        );
        let mut ui = simulator(view(&fixture.state));
        let child = bounds_of(&mut ui, format!("session-{}", fixture.child));
        let repository = text_within(&mut ui, "api", child).unwrap();
        assert!((text.x - repository.x).abs() < 0.5);
        assert!((summary.height - SUMMARY_HEIGHT).abs() < 0.5);
    }

    #[test]
    fn f2_renames_the_cursor_row() {
        use crate::app::{BareKey, Message};

        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        state.projects.cursor = Some(first.clone());
        let press = |state: &mut State, key| {
            let _task = update(state, Message::Key(key));
        };
        let field = Id::from(format!("rename-{first}"));

        press(&mut state, BareKey::F2);
        {
            let mut ui = simulator(view(&state));
            ui.find(field.clone()).unwrap();
            // The field takes what is typed, Enter included.
            ui.click(field.clone()).unwrap();
            assert_eq!(ui.typewrite("n"), iced::event::Status::Captured);
            assert_eq!(
                ui.tap_key(iced::keyboard::key::Named::Enter),
                iced::event::Status::Captured
            );
            let messages: Vec<_> = ui.into_messages().collect();
            // The click lands right of the short title, so the letter goes to its end.
            assert!(
                messages.contains(&Message::Projects(ProjectsMessage::RenameInput(
                    "untitledn".into()
                )))
            );
            assert!(messages.contains(&Message::Projects(ProjectsMessage::CommitRename)));
        }
        // A bare N while the field is open starts no session.
        press(&mut state, BareKey::N);
        assert_eq!(state.app.draft(), None);
        apply(
            &mut state,
            [ProjectsMessage::RenameInput("New name".into())],
        );
        press(&mut state, BareKey::Enter);
        assert_eq!(node(&state, &first).title, "New name");
        assert_eq!(state.projects.renaming, None);
        assert_eq!(state.tab, Tab::Projects);
        assert!(simulator(view(&state)).find(field.clone()).is_err());

        press(&mut state, BareKey::F2);
        apply(&mut state, [ProjectsMessage::RenameInput("Other".into())]);
        press(&mut state, BareKey::Escape);
        assert_eq!(node(&state, &first).title, "New name");
        assert_eq!(state.projects.renaming, None);
    }

    #[test]
    fn f2_without_a_cursor_or_on_another_tab_does_nothing() {
        use crate::app::{BareKey, Message};

        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        let _task = update(&mut state, Message::Key(BareKey::F2));
        assert_eq!(state.projects.renaming, None);

        state.projects.cursor = Some(first);
        state.tab = Tab::Session;
        let _task = update(&mut state, Message::Key(BareKey::F2));
        assert_eq!(state.projects.renaming, None);
    }

    #[test]
    fn a_double_click_on_the_title_renames_the_session() {
        let dir = project_dir();
        // The newer session is the top row, whose title is found first.
        let (state, _backend, [_, second]) = state_with_project(&dir);

        let mut ui = simulator(view(&state));
        ui.click("untitled").unwrap();
        ui.click("untitled").unwrap();
        let messages: Vec<_> = ui.into_messages().collect();
        let projects = |message| crate::app::Message::Projects(message);
        assert_eq!(
            messages
                .iter()
                .filter(|message| matches!(
                    message,
                    crate::app::Message::Projects(
                        ProjectsMessage::Highlight(_)
                            | ProjectsMessage::StartRename(_)
                            | ProjectsMessage::Select(_)
                    )
                ))
                .cloned()
                .collect::<Vec<_>>(),
            [
                projects(ProjectsMessage::Highlight(second.clone())),
                projects(ProjectsMessage::Highlight(second.clone())),
                projects(ProjectsMessage::StartRename(second)),
            ]
        );
    }

    #[test]
    fn a_blank_rename_keeps_the_title() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        apply(
            &mut state,
            [
                ProjectsMessage::StartRename(first.clone()),
                ProjectsMessage::RenameInput("   ".into()),
                ProjectsMessage::CommitRename,
            ],
        );
        assert_eq!(node(&state, &first).title, "untitled");
        assert_eq!(state.projects.renaming, None);
        assert_eq!(state.error, None);
    }

    #[test]
    fn a_long_title_does_not_wrap() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        state
            .app
            .rename_session(first.clone(), &"word ".repeat(40))
            .unwrap();

        let mut ui = simulator(view(&state));
        let row = bounds_of(&mut ui, format!("session-{first}"));
        assert!((row.height - ROW_HEIGHT).abs() < 0.5);
    }

    #[test]
    fn the_action_strip_keeps_the_columns_in_place() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        let pill_x = |state: &State| {
            let mut ui = simulator(view(state));
            let row = bounds_of(&mut ui, format!("session-{first}"));
            text_within(&mut ui, "idle", row).unwrap().x
        };
        let calm = pill_x(&state);
        state.projects.hovered = Some(first.clone());
        let hovered = pill_x(&state);
        simulator(view(&state))
            .find(Id::from(format!("archive-{first}")))
            .unwrap();
        assert!((calm - hovered).abs() < 0.5);
    }

    #[test]
    fn n_after_folding_the_cursors_project_starts_a_session_there() {
        let dir = project_dir();
        let (mut fixture, other) = orchestrated_and_another_project(&dir);
        let tree = fixture.state.app.tree();
        let second = tree.projects[1].clone();
        fixture.state.tab = Tab::Projects;
        fixture.state.projects.cursor = Some(other);

        apply(
            &mut fixture.state,
            [
                ProjectsMessage::ToggleProject(second.id.clone()),
                ProjectsMessage::NewSessionAtCursor,
            ],
        );

        assert_eq!(fixture.state.projects.cursor, None);
        assert_eq!(fixture.state.app.draft().unwrap().project, second.name);
    }

    #[test]
    fn a_new_cursor_forgets_the_folded_project() {
        let dir = project_dir();
        let (mut fixture, other) = orchestrated_and_another_project(&dir);
        let tree = fixture.state.app.tree();
        let (first, second) = (tree.projects[0].clone(), tree.projects[1].clone());
        fixture.state.tab = Tab::Projects;
        fixture.state.projects.cursor = Some(other);
        apply(
            &mut fixture.state,
            [ProjectsMessage::ToggleProject(second.id.clone())],
        );
        assert_eq!(fixture.state.projects.fallback_project, Some(second.id));

        apply(&mut fixture.state, [ProjectsMessage::CursorDown]);
        assert_eq!(fixture.state.projects.fallback_project, None);
        apply(&mut fixture.state, [ProjectsMessage::NewSessionAtCursor]);
        assert_eq!(fixture.state.app.draft().unwrap().project, first.name);
    }

    #[test]
    fn closing_the_folded_project_forgets_it() {
        let dir = project_dir();
        let (mut fixture, other) = orchestrated_and_another_project(&dir);
        let tree = fixture.state.app.tree();
        let (first, second) = (tree.projects[0].clone(), tree.projects[1].clone());
        fixture.state.tab = Tab::Projects;
        fixture.state.projects.cursor = Some(other);
        apply(
            &mut fixture.state,
            [
                ProjectsMessage::ToggleProject(second.id.clone()),
                ProjectsMessage::Close(second.id),
                ProjectsMessage::NewSessionAtCursor,
            ],
        );
        assert_eq!(fixture.state.projects.fallback_project, None);
        assert_eq!(fixture.state.app.draft().unwrap().project, first.name);
    }

    #[test]
    fn a_crowded_row_keeps_its_action_strip_inside() {
        let mut fixture = orchestrated_after(Some("/brainstorming Plan the split"));
        let orchestrator = fixture.orchestrator.clone();
        fixture
            .state
            .app
            .rename_session(orchestrator.clone(), &"x".repeat(TITLE_BUDGET))
            .unwrap();
        deliver(
            &mut fixture.state,
            BackendEvent::Update {
                session: fixture.child.clone(),
                update: asks_permission(),
            },
        );
        let pill_x = |state: &State| {
            let mut ui = simulator(view(state));
            let row = bounds_of(&mut ui, format!("session-{orchestrator}"));
            text_within(&mut ui, "running", row).unwrap().x
        };
        let calm = pill_x(&fixture.state);

        fixture.state.projects.hovered = Some(orchestrator.clone());
        let mut ui = simulator(view(&fixture.state));
        let row = bounds_of(&mut ui, format!("session-{orchestrator}"));
        assert!(shows_within(&mut ui, "/brainstorming", row));
        assert!(shows_within(&mut ui, badge::ORCHESTRATOR, row));
        assert!(shows_within(&mut ui, CHILD_ASKS_YOU, row));
        for strip in ["archive", "delete"] {
            let button = bounds_of(&mut ui, format!("{strip}-{orchestrator}"));
            assert!(button.x >= row.x && button.x + button.width <= row.x + row.width);
        }
        drop(ui);
        assert!((pill_x(&fixture.state) - calm).abs() < 0.5);
    }

    #[test]
    fn highlighting_another_row_closes_the_title_field() {
        use crate::app::{BareKey, Message};

        let dir = project_dir();
        let (mut state, _backend, [first, second]) = state_with_project(&dir);
        apply(
            &mut state,
            [
                ProjectsMessage::StartRename(first.clone()),
                ProjectsMessage::Highlight(second.clone()),
            ],
        );
        assert_eq!(state.projects.renaming, None);

        // The keys reach the tree again: the cursor moves down to the older session.
        let _task = update(&mut state, Message::Key(BareKey::ArrowDown));
        assert_eq!(state.projects.cursor, Some(first.clone()));
        assert!(
            simulator(view(&state))
                .find(Id::from(format!("rename-{first}")))
                .is_err()
        );

        // Renaming the row that is already being renamed keeps the field.
        apply(
            &mut state,
            [
                ProjectsMessage::StartRename(first.clone()),
                ProjectsMessage::RenameInput("Kept".into()),
                ProjectsMessage::Highlight(first.clone()),
            ],
        );
        assert_eq!(
            state.projects.renaming,
            Some((first.clone(), "Kept".into()))
        );

        // Picking another row closes it.
        apply(&mut state, [ProjectsMessage::Select(second)]);
        assert_eq!(state.projects.renaming, None);
        assert_eq!(node(&state, &first).title, "untitled");
    }

    #[test]
    fn changing_the_tab_closes_the_title_field() {
        use crate::app::{BareKey, Message};

        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        apply(&mut state, [ProjectsMessage::StartRename(first.clone())]);

        let _task = update(&mut state, Message::SelectTab(Tab::Session));
        assert_eq!(state.projects.renaming, None);
        let _task = update(&mut state, Message::SelectTab(Tab::Projects));
        state.projects.cursor = Some(first.clone());
        let _task = update(&mut state, Message::Key(BareKey::N));
        assert!(state.app.draft().is_some());
    }

    #[test]
    fn folding_the_project_closes_the_title_field() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        let project = state.app.tree().projects[0].id.clone();
        apply(
            &mut state,
            [
                ProjectsMessage::StartRename(first),
                ProjectsMessage::ToggleProject(project),
            ],
        );
        assert_eq!(state.projects.renaming, None);
    }

    #[test]
    fn renaming_a_child_keeps_its_repository() {
        let mut fixture = orchestrated();
        expand(&mut fixture);
        let child = fixture.child.clone();
        apply(
            &mut fixture.state,
            [ProjectsMessage::StartRename(child.clone())],
        );

        let mut ui = simulator(view(&fixture.state));
        let row = bounds_of(&mut ui, format!("session-{child}"));
        let repository = text_within(&mut ui, "api", row).unwrap();
        let field = bounds_of(&mut ui, format!("rename-{child}"));
        assert!(repository.x < field.x);
    }

    #[test]
    fn an_idle_session_shows_its_known_context() {
        let dir = project_dir();
        // The newer session is current, so its transcript, which knows the usage, is loaded.
        let (mut state, backend, [_, first]) = state_with_project(&dir);
        attach(&mut state, &backend, &first, "b-first");
        report(
            &mut state,
            &first,
            Update::Usage {
                used: 68_000,
                size: 1_000_000,
            },
        );

        let mut ui = simulator(view(&state));
        let row = bounds_of(&mut ui, format!("session-{first}"));
        assert!(shows_within(&mut ui, "idle", row));
        assert!(shows_within(&mut ui, "68k / 1M", row));
    }
}
