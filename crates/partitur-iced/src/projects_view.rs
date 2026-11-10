//! The Projects tab: the tree of open projects with their sessions, over a status line.
//!
//! The tree fills the window's width and scrolls; the status line stays at the bottom. A
//! project starts with its header: its name, its folder, how many repositories it has, the
//! instruction files (`AGENTS.md`, `CLAUDE.md`) its folder holds, and a close button on the
//! right. Under it come its sessions as rows: a chevron when sessions were spawned from the
//! session (it hides and shows them), the title, the repository the session works in, a pill
//! for an orchestrator, a status pill (none while idle) and a pill when a session below
//! waits for the user. A spawned session sits one level deeper than its parent and leads
//! with its repository. On the right a row tells how many of its children are done or, for
//! a session without children, how much of its context it has used, once that is known.
//! One row is highlighted: the cursor, which follows the selected session and which the
//! arrow keys move. The highlighted row and the row under the pointer end in an action strip
//! with the archive and the delete button, which read `Stop and archive` and `Stop and
//! delete` while the session runs or waits and cannot be pressed on a child session, which
//! goes with its orchestrator. A project ends with a `new session` row and, one level
//! deeper, an `in <repository>` row for every repository other than the project root.
//! Between the two, a project with archived sessions has an `archived · N` row, collapsed at
//! first, that shows and hides them: each with its title, when it was archived and the
//! `restore` and `delete` links, the sessions archived with it one level deeper. The tree's
//! last row opens another project folder.
//!
//! Deleting a session moves the cursor to the nearest row still shown and forgets what the
//! interface remembered about the deleted sessions; when the Session or Env tab showed one
//! of them, the Projects tab is shown instead.
//!
//! The status line counts the live sessions that run, wait for the user, failed or are done,
//! and the archived ones, and names the keys the tab answers to.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use iced::widget::{
    Column, Row, Space, button, column, container, mouse_area, row, scrollable, space, text,
    tooltip,
};
use iced::{Alignment, Color, Element, Length, Padding, Task};
use partitur_core::{
    ProjectId, ProjectNode, RepositoryNode, Scope, SessionId, SessionNode, Status, TreeModel,
    day_month,
};

use crate::app::{Message, State, Tab, report};
use crate::confirm_sheet::{self, Action};
use crate::ellipsis::{keep_end, keep_start};
use crate::icon::{self, Icon, icon};
use crate::theme::Tokens;
use crate::{badge, home_path, session_view, style, theme, tokens_label};

/// The height of a row of the tree, in logical pixels.
const ROW_HEIGHT: f32 = 28.0;

/// The vertical space between the rows of one project.
const ROW_GAP: f32 = 2.0;

/// The vertical space between one project and the next.
const PROJECT_GAP: f32 = 22.0;

/// The space above and below the tree.
const TREE_MARGIN_Y: f32 = 28.0;

/// The space between the window's edge and the tree's text, left and right.
const TREE_MARGIN_X: f32 = 40.0;

/// How far a row's background reaches beyond the tree's text edge on each side, so the
/// highlighted row frames its text instead of touching it.
const BLEED: f32 = 12.0;

/// How far a top-level session row and the `new session` row sit right of the project
/// header.
const SESSION_INDENT: f32 = 20.0;

/// How far each level of spawned sessions sits right of its parent.
const LEVEL_INDENT: f32 = 32.0;

/// How far the slot at the left of a row reaches beyond its icon on each side. The slot is
/// what a click on a chevron has to hit, and the icon alone would be too small a target.
const SLOT_REACH: f32 = 4.0;

/// The width of the slot at the left of a row: the icon and its reach on both sides.
const SLOT_WIDTH: f32 = icon::SIZE + 2.0 * SLOT_REACH;

/// The space between the parts of a session row.
const PART_GAP: f32 = 10.0;

/// The space between the parts of a project header.
const HEADER_GAP: f32 = 12.0;

/// The padding around the close icon, which makes its button large enough to hit.
const CLOSE_PADDING: f32 = 6.0;

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

/// The keys the status line names, each with what it does on this tab.
const KEY_HINTS: [(&str, &str); 5] = [
    ("A", "archive"),
    ("D", "delete"),
    ("Enter", "open"),
    ("Tab", "Session / Env"),
    ("N", "new session"),
];

/// A colour picked from the active theme's tokens.
type Ink = fn(&Tokens) -> Color;

/// What the Projects tab remembers between frames. At start no session is collapsed, and
/// the application puts the cursor on its current session.
#[derive(Debug, Default)]
pub struct ProjectsState {
    /// The sessions whose spawned sessions are hidden; every other session shows them.
    pub(crate) collapsed: HashSet<SessionId>,
    /// The session of the highlighted row, which the arrow keys move and Enter opens.
    pub(crate) cursor: Option<SessionId>,
    /// The projects whose archived sessions are shown; every other project hides them.
    pub(crate) archived_open: HashSet<ProjectId>,
    /// The live session whose row the pointer is over.
    pub(crate) hovered: Option<SessionId>,
}

/// An interaction on the Projects tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectsMessage {
    /// The user asked to open a project folder; the folder dialog opens.
    OpenFolder,
    /// The folder dialog closed: with the chosen folder, or `None` when it was cancelled.
    FolderPicked(Option<PathBuf>),
    /// The user asked for a new session in a project, for the given scope.
    NewSession(ProjectId, Scope),
    /// The user picked a session.
    Select(SessionId),
    /// The user closed a project.
    Close(ProjectId),
    /// The user hid or showed the sessions spawned from a session.
    Toggle(SessionId),
    /// The user hid or showed a project's archived sessions.
    ToggleArchived(ProjectId),
    /// The user restored an archived session.
    Restore(SessionId),
    /// The user asked to archive a top-level session: the sheet that confirms it opens.
    Archive(SessionId),
    /// The user asked to delete a top-level session, live or archived: the sheet that
    /// confirms it opens.
    Delete(SessionId),
    /// The pointer entered the row of a live session.
    Hover(SessionId),
    /// The pointer left the row of a live session.
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
    /// The user pressed N: a new session of the whole project starts in the cursor's
    /// project.
    NewSessionAtCursor,
}

/// The Projects tab: every project of `tree` with its sessions and repositories, drawn with
/// the collapsed sessions and the cursor of `state`, over the status line.
///
/// The element owns what it shows, so it does not borrow `tree`, which the caller builds
/// afresh for every frame.
pub fn view<'a>(tree: &TreeModel, state: &ProjectsState) -> Element<'a, Message> {
    let home = std::env::home_dir();
    // The rows' backgrounds reach `BLEED` beyond the text edge, so the tree keeps that much
    // less margin and every row pads its content by it.
    let mut content = Column::new()
        .spacing(PROJECT_GAP)
        .padding([TREE_MARGIN_Y, TREE_MARGIN_X - BLEED]);
    if tree.projects.is_empty() {
        content = content
            .push(container(text("No projects open").style(style::muted)).padding([0.0, BLEED]));
    }
    for project in &tree.projects {
        content = content.push(project_section(project, state, home.as_deref()));
    }
    content = content.push(open_folder_row());
    column![
        scrollable(content).width(Length::Fill).height(Length::Fill),
        status_line(tree),
    ]
    .into()
}

/// Applies `message` to the Projects tab and returns the follow-up work.
///
/// Opening, creating, selecting and closing go through the core [`App`](partitur_core::App);
/// when it refuses, the error bar shows why. Creating or selecting a session switches to
/// the Session tab, where the session is now current, moves the cursor onto it and expands
/// the collapsed sessions above it; closing
/// a project forgets what the interface remembers about its sessions. Archiving and
/// deleting open the sheet that confirms them. The key presses (moving the cursor, opening
/// its session, starting a session in its project, archiving or deleting its session) count
/// only while the Projects tab is shown.
pub fn update(state: &mut State, message: ProjectsMessage) -> Task<Message> {
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
            if report(&mut state.error, state.app.new_session(project, scope)).is_some() {
                show_current_session(state);
            }
        }
        ProjectsMessage::Select(session) => {
            if report(&mut state.error, state.app.select_session(session)).is_some() {
                show_current_session(state);
            }
        }
        ProjectsMessage::Close(project) => close(state, project),
        ProjectsMessage::Toggle(session) => toggle(state, session),
        ProjectsMessage::ToggleArchived(project) => toggle_archived(state, project),
        ProjectsMessage::Restore(session) => {
            report(&mut state.error, state.app.restore(session));
        }
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
/// cursor onto it. Every collapsed session above it shows its spawned sessions again, so
/// the highlighted row is in sight when the user comes back to the tree.
fn show_current_session(state: &mut State) {
    state.tab = Tab::Session;
    state.projects.cursor = state.app.current_session();
    if let Some(current) = state.projects.cursor.clone() {
        let current = &current;
        let tree = state.app.tree();
        let mut above = Vec::new();
        for project in &tree.projects {
            if sessions_above(&project.sessions, current, &mut above) {
                break;
            }
            if sessions_above(&project.archived, current, &mut above) {
                state.projects.archived_open.insert(project.id.clone());
                break;
            }
        }
        for session in above {
            state.projects.collapsed.remove(&session);
        }
    }
    session_view::sync_markdown(state);
}

/// Looks for `target` among `sessions` and the sessions below them. When it is found,
/// `above` holds the sessions it was spawned from, outermost first, and the result is
/// `true`; else `above` is left as it was.
fn sessions_above(
    sessions: &[SessionNode],
    target: &SessionId,
    above: &mut Vec<SessionId>,
) -> bool {
    for session in sessions {
        if &session.id == target {
            return true;
        }
        above.push(session.id.clone());
        if sessions_above(&session.children, target, above) {
            return true;
        }
        above.pop();
    }
    false
}

/// Closes `project` and forgets what the interface remembers about its sessions, the
/// cursor included.
fn close(state: &mut State, project: ProjectId) {
    let closing = project_sessions(&state.app.tree(), &project);
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
}

/// Forgets what the interface remembers about `sessions`, which left the tree: parsed
/// answers, expanded entries, approval card drafts, collapsed and hovered rows.
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
    state
        .projects
        .collapsed
        .retain(|session| !sessions.contains(session));
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
    }
}

/// Hides the sessions spawned from `session`, or shows them when they are hidden. A cursor
/// on a row this hides moves to `session`, so the highlighted row stays in sight.
fn toggle(state: &mut State, session: SessionId) {
    if !state.projects.collapsed.remove(&session) {
        state.projects.collapsed.insert(session.clone());
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

/// Deletes `session` with the sessions below it and reports whether they are gone. They
/// can be gone with an error, when closing their backend sessions or deleting their
/// transcripts failed; the error bar says so. A cursor on a deleted row moves to the nearest
/// row still shown, below it first, and the interface forgets what it remembered about the
/// deleted sessions. When the Session or Env tab showed one of them, the Projects tab is
/// shown instead.
pub(crate) fn delete(state: &mut State, session: SessionId) -> bool {
    let tree = state.app.tree();
    let before = visible_sessions(&tree, &state.projects);
    let mut family = Vec::new();
    if let Some(node) = top_level(&tree, &session) {
        collect_sessions(std::slice::from_ref(node), &HashSet::new(), &mut family);
    }
    let showed_one = state
        .app
        .current_session()
        .is_some_and(|current| family.contains(&current));
    let result = state.app.delete(session);
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

/// Hides the archived sessions of `project`, or shows them when they are hidden. A cursor on
/// a row this hides leaves the tree.
fn toggle_archived(state: &mut State, project: ProjectId) {
    if !state.projects.archived_open.remove(&project) {
        state.projects.archived_open.insert(project);
    }
    let visible = visible_sessions(&state.app.tree(), &state.projects);
    if state
        .projects
        .cursor
        .as_ref()
        .is_some_and(|cursor| !visible.contains(cursor))
    {
        state.projects.cursor = None;
    }
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

/// The project a session started from the keyboard goes to: the cursor's project, or the
/// first project when there is no cursor.
fn cursor_project(state: &State) -> Option<ProjectId> {
    let tree = state.app.tree();
    let of_cursor = state.projects.cursor.as_ref().and_then(|cursor| {
        tree.projects
            .iter()
            .find(|project| project_sessions(&tree, &project.id).contains(cursor))
    });
    of_cursor
        .or_else(|| tree.projects.first())
        .map(|project| project.id.clone())
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
        collect_sessions(&project.sessions, &HashSet::new(), &mut ids);
        collect_sessions(&project.archived, &HashSet::new(), &mut ids);
    }
    ids
}

/// The sessions whose rows the tree shows, top to bottom: every live session of `tree`, and
/// the archived ones of the projects whose archived sessions `state` shows, that no
/// collapsed session above it hides.
fn visible_sessions(tree: &TreeModel, state: &ProjectsState) -> Vec<SessionId> {
    let mut ids = Vec::new();
    for project in &tree.projects {
        collect_sessions(&project.sessions, &state.collapsed, &mut ids);
        if state.archived_open.contains(&project.id) {
            collect_sessions(&project.archived, &state.collapsed, &mut ids);
        }
    }
    ids
}

/// Appends `sessions` to `ids` in the tree's order, each followed by the sessions spawned
/// from it unless it is in `collapsed`.
fn collect_sessions(
    sessions: &[SessionNode],
    collapsed: &HashSet<SessionId>,
    ids: &mut Vec<SessionId>,
) {
    for session in sessions {
        ids.push(session.id.clone());
        if !collapsed.contains(&session.id) {
            collect_sessions(&session.children, collapsed, ids);
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

/// One project: its header, its session rows, the `new session` row, the archived sessions'
/// row with the archived sessions under it while they are shown, and a row per repository
/// one level deeper.
fn project_section<'a>(
    project: &ProjectNode,
    state: &ProjectsState,
    home: Option<&Path>,
) -> Element<'a, Message> {
    let mut section = Column::new()
        .spacing(ROW_GAP)
        .push(project_header(project, home));
    for session in &project.sessions {
        section = push_session(section, session, 0, state);
    }
    section = section.push(new_session_row(
        project,
        Scope::Project,
        format!("new-session-{}", project.id),
        row![text("new session").style(style::muted)],
        0,
    ));
    if !project.archived.is_empty() {
        let open = state.archived_open.contains(&project.id);
        section = section.push(archived_group_row(project, open));
        if open {
            for session in &project.archived {
                section = push_archived(section, session, 1, state);
            }
        }
    }
    for repository in &project.repositories {
        section = section.push(repository_row(project, repository));
    }
    section.into()
}

/// The project's name, folder, repository count and instruction files, found by
/// `project-<id>`, with the close button at the right edge, found by `close-<id>`.
fn project_header<'a>(project: &ProjectNode, home: Option<&Path>) -> Element<'a, Message> {
    let folder = keep_end(&home_path::abbreviated(&project.root, home), PATH_BUDGET);
    let mut header = row![
        text(project.name.clone()).font(theme::UI_SEMIBOLD),
        mono(folder),
        separator(),
        text(repository_count(project.repository_count)).style(style::muted),
    ]
    .spacing(HEADER_GAP)
    .align_y(Alignment::Center);
    for file in instruction_files(
        project.has_global_instructions,
        project.has_global_claude_md,
    ) {
        header = header.push(separator()).push(mono(file));
    }
    let close = button(icon(Icon::Close, |tokens| tokens.muted))
        .padding(CLOSE_PADDING)
        .style(style::row_button)
        .on_press(Message::Projects(ProjectsMessage::Close(
            project.id.clone(),
        )));
    container(
        row![
            container(header).id(format!("project-{}", project.id)),
            space::horizontal(),
            container(close).id(format!("close-{}", project.id)),
        ]
        .align_y(Alignment::Center),
    )
    .center_y(ROW_HEIGHT)
    // The close icon, not its button, ends at the text edge.
    .padding(Padding::ZERO.left(BLEED).right(BLEED - CLOSE_PADDING))
    .into()
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
/// rows of the sessions spawned from it, unless it is collapsed.
fn push_session<'a>(
    mut section: Column<'a, Message>,
    session: &SessionNode,
    depth: u16,
    state: &ProjectsState,
) -> Column<'a, Message> {
    section = section.push(session_row(session, depth, state));
    if !state.collapsed.contains(&session.id) {
        for child in &session.children {
            section = push_session(section, child, depth.saturating_add(1), state);
        }
    }
    section
}

/// The row of `session`, found by `session-<id>`: the chevron that hides or shows its
/// spawned sessions (found by `toggle-<id>`), its labels and pills, and its remark at the
/// right edge. A click anywhere but on the chevron picks the session. The cursor's row is
/// highlighted.
fn session_row<'a>(
    session: &SessionNode,
    depth: u16,
    state: &ProjectsState,
) -> Element<'a, Message> {
    let highlighted = state.cursor.as_ref() == Some(&session.id);
    // The weaker muted colour is too faint on the selection colour.
    let (main_ink, muted_ink): (Ink, Ink) = if highlighted {
        (|tokens| tokens.text_strong, |tokens| tokens.muted_strong)
    } else {
        (|tokens| tokens.text_soft, |tokens| tokens.muted)
    };
    let font = if highlighted {
        theme::UI_MEDIUM
    } else {
        theme::UI
    };
    let title = keep_start(&session.title, TITLE_BUDGET);
    // A spawned session is one of several that share their parent's task, so what tells
    // them apart, the repository, leads.
    let (main, secondary) = if depth > 0 && !session.scope_label.is_empty() {
        (session.scope_label.clone(), title)
    } else {
        (title, session.scope_label.clone())
    };

    let mut line = Row::new().push(text(main).font(font).style(style::ink(main_ink)));
    if !secondary.is_empty() {
        line = line.push(text(secondary).style(style::ink(muted_ink)));
    }
    if session.orchestrator {
        line = line.push(badge::orchestrator());
    }
    if let Some(status) = badge::status(session.status) {
        line = line.push(status);
    }
    if session.attention && session.status != Status::WaitingForYou {
        line = line.push(badge::pill(CHILD_ASKS_YOU, |tokens| tokens.asks_you));
    }
    line = line.push(space::horizontal());
    if let Some(remark) = remark(session, muted_ink) {
        line = line.push(remark);
    }
    if highlighted || state.hovered.as_ref() == Some(&session.id) {
        line = line.push(action_strip(session, depth));
    }

    let row = tree_row(
        format!("session-{}", session.id),
        SESSION_INDENT + f32::from(depth) * LEVEL_INDENT,
        chevron(session, state.collapsed.contains(&session.id), muted_ink),
        line,
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
/// shown and right while `collapsed` hides them.
fn chevron<'a>(session: &SessionNode, collapsed: bool, ink: Ink) -> Element<'a, Message> {
    if session.children.is_empty() {
        return Space::new().width(SLOT_WIDTH).into();
    }
    let direction = if collapsed {
        Icon::ChevronRight
    } else {
        Icon::ChevronDown
    };
    let toggle = button(container(icon(direction, ink)).center(Length::Fill))
        .padding(0)
        .width(SLOT_WIDTH)
        .height(Length::Fill)
        .style(style::link_button)
        .on_press(Message::Projects(ProjectsMessage::Toggle(
            session.id.clone(),
        )));
    container(toggle)
        .id(format!("toggle-{}", session.id))
        .into()
}

/// What the right edge of a session's row tells: how many of the sessions spawned from it
/// are done, or, for a session without any, its context usage once that is known.
fn remark<'a>(session: &SessionNode, ink: Ink) -> Option<Element<'a, Message>> {
    if session.children.is_empty() {
        let (used, size) = session.usage?;
        let usage = text(tokens_label::usage(used, size)).font(theme::MONO);
        return Some(usage.size(theme::SMALL).style(style::ink(ink)).into());
    }
    let done = session
        .children
        .iter()
        .filter(|child| child.status == Status::Done)
        .count();
    let progress = format!("{done} of {} children done", session.children.len());
    Some(
        text(progress)
            .size(theme::SMALL)
            .style(style::ink(ink))
            .into(),
    )
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
    let line = row![
        icon(Icon::Archive, |tokens| tokens.dim),
        text(format!("archived · {}", project.archived.len())).style(style::muted),
    ];
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
/// under it the rows of the sessions archived with it, unless it is collapsed.
fn push_archived<'a>(
    mut section: Column<'a, Message>,
    session: &SessionNode,
    depth: u16,
    state: &ProjectsState,
) -> Column<'a, Message> {
    section = section.push(archived_row(session, depth, state));
    if !state.collapsed.contains(&session.id) {
        for child in &session.children {
            section = push_archived(section, child, depth.saturating_add(1), state);
        }
    }
    section
}

/// The row of an archived session, found by `session-<id>`: its title, when it was archived
/// and, for a session archived on its own account, the `restore` and `delete` links at the
/// right edge (found by `restore-<id>` and `delete-<id>`). A click anywhere else opens the
/// session read-only.
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
    if depth == 1 {
        let link_to = |label, message| {
            button(text(label).size(theme::SMALL))
                .padding(0)
                .style(style::link_button)
                .on_press(Message::Projects(message))
        };
        let restore = link_to("restore", ProjectsMessage::Restore(session.id.clone()));
        let delete = link_to("delete", ProjectsMessage::Delete(session.id.clone()));
        line = line
            .push(container(restore).id(format!("restore-{}", session.id)))
            .push(container(delete).id(format!("delete-{}", session.id)));
    }
    tree_row(
        format!("session-{}", session.id),
        SESSION_INDENT + f32::from(depth) * LEVEL_INDENT,
        chevron(session, state.collapsed.contains(&session.id), meta_ink),
        line,
        highlighted,
        ProjectsMessage::Select(session.id.clone()),
    )
}

/// The row that starts a session in `repository`, one level deeper than the `new session`
/// row: `in` and its name, then the names of the instruction files it holds.
fn repository_row<'a>(project: &ProjectNode, repository: &RepositoryNode) -> Element<'a, Message> {
    let mut label = row![
        text(format!("in {}", repository.name))
            .size(theme::SMALL)
            .style(style::muted)
    ];
    for file in instruction_files(repository.has_instructions, repository.has_claude_md) {
        label = label.push(mono(file));
    }
    new_session_row(
        project,
        Scope::Repository(repository.id.clone()),
        format!("new-session-{}-{}", project.id, repository.id),
        label,
        1,
    )
}

/// A row with a plus and `label`, `depth` levels below the top-level sessions, that starts
/// a session of `scope` in `project`, found by `id`.
fn new_session_row<'a>(
    project: &ProjectNode,
    scope: Scope,
    id: String,
    label: Row<'a, Message>,
    depth: u16,
) -> Element<'a, Message> {
    tree_row(
        id,
        SESSION_INDENT + f32::from(depth) * LEVEL_INDENT,
        plus(),
        label,
        false,
        ProjectsMessage::NewSession(project.id.clone(), scope),
    )
}

/// The tree's last row, level with the project headers, which opens the folder dialog.
fn open_folder_row<'a>() -> Element<'a, Message> {
    tree_row(
        "open-project-folder".to_owned(),
        0.0,
        plus(),
        row![text("open project folder").style(style::muted)],
        false,
        ProjectsMessage::OpenFolder,
    )
}

/// One clickable row of the tree, found by `id`: `slot` then the parts of `line`, with the
/// slot's icon `indent` right of the project headers. A click that `slot` does not take sends
/// `on_press`. The row's background reaches [`BLEED`] beyond the text on both sides: the
/// selection colour when `highlighted`, else a faint one while hovered.
fn tree_row<'a>(
    id: String,
    indent: f32,
    slot: Element<'a, Message>,
    line: Row<'a, Message>,
    highlighted: bool,
    on_press: ProjectsMessage,
) -> Element<'a, Message> {
    // The slot reaches beyond its icon, so it starts that much further left and the line
    // follows that much closer.
    let content = row![slot, line.spacing(PART_GAP).align_y(Alignment::Center)]
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

/// The plus in the slot of a row that adds something.
fn plus<'a>() -> Element<'a, Message> {
    container(icon(Icon::Plus, |tokens| tokens.dim))
        .center_x(SLOT_WIDTH)
        .into()
}

/// A path or file name: monospaced, small and muted.
fn mono<'a>(label: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    text(label)
        .font(theme::MONO)
        .size(theme::SMALL)
        .style(style::muted)
        .into()
}

/// The dot between the facts of a project header.
fn separator<'a>() -> Element<'a, Message> {
    text("·").style(style::dim).into()
}

/// The line at the bottom of the tab: on the left how many live sessions of `tree` run, wait
/// for the user, failed and are done, then how many are archived (each only when there is
/// one), on the right the keys.
fn status_line<'a>(tree: &TreeModel) -> Element<'a, Message> {
    let counted: [(Status, &str, Option<Ink>); 4] = [
        (Status::Running, "running", Some(|tokens| tokens.running.fg)),
        (
            Status::WaitingForYou,
            "asks you",
            Some(|tokens| tokens.asks_you.fg),
        ),
        (Status::Failed, "failed", Some(|tokens| tokens.failed.fg)),
        (Status::Done, "done", None),
    ];
    let mut line = Row::new().spacing(18).align_y(Alignment::Center);
    for (status, label, dot) in counted {
        let count: usize = tree
            .projects
            .iter()
            .map(|project| sessions_showing(&project.sessions, status))
            .sum();
        if count == 0 {
            continue;
        }
        let mut part = Row::new().spacing(6).align_y(Alignment::Center);
        if let Some(dot) = dot {
            part = part.push(badge::dot(dot));
        }
        line = line.push(part.push(small(format!("{count} {label}"))));
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
                .style(style::muted),
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
    use partitur_core::testing::{Call, MemoryStore, RecordingBackend, single_claude};
    use partitur_core::{
        App, BackendEvent, PermissionId, PermissionKind, PermissionOption, PermissionOptionId,
        ProjectId, RepositoryId, Scope, SessionId, StopReason, ToolCallId, ToolKind, Update,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::app::{State, Tab, boot, update, view};
    use crate::test_support::{Orchestrated, attach, deliver, orchestrated};

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
            backends,
            default,
        )
        .unwrap();
        (boot(app, mpsc::unbounded().1).0, backend)
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

    /// Feeds the messages through `update`, dropping the tasks: the simulator cannot run them.
    fn apply(state: &mut State, messages: impl IntoIterator<Item = ProjectsMessage>) {
        for message in messages {
            let _task = update(state, crate::app::Message::Projects(message));
        }
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
    fn an_open_project_lists_its_sessions_and_repositories() {
        let dir = project_dir();
        let (state, _backend, [first, second]) = state_with_project(&dir);
        let project = state.app.tree().projects[0].clone();
        let folder = home_path::abbreviated(&project.root, std::env::home_dir().as_deref());
        let mut ui = simulator(view(&state));

        ui.find(project.name.as_str()).unwrap();
        ui.find(folder.as_str()).unwrap();
        ui.find("1 repo").unwrap();
        ui.find("new session").unwrap();
        ui.find("in api").unwrap();
        ui.find(Id::from(format!("session-{first}"))).unwrap();
        ui.find(Id::from(format!("session-{second}"))).unwrap();
        ui.find(Id::from(format!("new-session-{}", project.id)))
            .unwrap();
        ui.find(Id::from(format!(
            "new-session-{}-{}",
            project.id, project.repositories[0].id
        )))
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
        let in_api = bounds_of(&mut ui, format!("new-session-{project}-api"));
        assert!(shows_within(&mut ui, "CLAUDE.md", header));
        assert!(!shows_within(&mut ui, "CLAUDE.md", in_api));
        assert!(ui.find("AGENTS.md").is_err());
    }

    #[test]
    fn a_repository_claude_md_is_named_on_its_row() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("api").join(".git")).unwrap();
        fs::write(dir.path().join("api").join("CLAUDE.md"), "api only").unwrap();
        let (state, _backend, _) = state_with_project(&dir);
        let project = state.app.tree().projects[0].id.clone();

        let mut ui = simulator(view(&state));
        let header = bounds_of(&mut ui, format!("project-{project}"));
        let in_api = bounds_of(&mut ui, format!("new-session-{project}-api"));
        assert!(shows_within(&mut ui, "CLAUDE.md", in_api));
        assert!(!shows_within(&mut ui, "CLAUDE.md", header));
        assert!(ui.find("AGENTS.md").is_err());
    }

    #[test]
    fn a_project_with_both_instruction_files_names_both_in_both_places() {
        let dir = project_dir();
        fs::write(dir.path().join("CLAUDE.md"), "global").unwrap();
        fs::write(dir.path().join("api").join("CLAUDE.md"), "api only").unwrap();
        let (state, _backend, _) = state_with_project(&dir);
        let project = state.app.tree().projects[0].id.clone();

        let mut ui = simulator(view(&state));
        let header = bounds_of(&mut ui, format!("project-{project}"));
        let in_api = bounds_of(&mut ui, format!("new-session-{project}-api"));
        for area in [header, in_api] {
            assert!(shows_within(&mut ui, "AGENTS.md", area));
            assert!(shows_within(&mut ui, "CLAUDE.md", area));
        }
    }

    #[test]
    fn a_repository_session_names_its_repository_after_its_title() {
        let dir = project_dir();
        let api = RepositoryId::try_from("api").unwrap();
        let (state, _, session) = state_with_session(&dir, Scope::Repository(api));

        let mut ui = simulator(view(&state));
        let row = bounds_of(&mut ui, format!("session-{session}"));
        let title = ui.find("New session").unwrap().bounds();
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
        let fixture = orchestrated();

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
        let fixture = orchestrated();
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
    fn an_idle_session_shows_no_status_badge_and_no_chevron() {
        let dir = project_dir();
        let (state, _backend, [first, _]) = state_with_project(&dir);

        let mut ui = simulator(view(&state));
        ui.find(Id::from(format!("session-{first}"))).unwrap();
        assert!(ui.find("idle").is_err());
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
    }

    #[test]
    fn every_repository_gets_its_own_new_session_row() {
        let fixture = orchestrated();
        let project = fixture.state.app.tree().projects[0].id.clone();

        let mut ui = simulator(view(&fixture.state));
        ui.find("new session").unwrap();
        ui.find("in api").unwrap();
        ui.find("in web").unwrap();
        ui.find("2 repos").unwrap();
        ui.find(Id::from(format!("new-session-{project}-api")))
            .unwrap();
        ui.find(Id::from(format!("new-session-{project}-web")))
            .unwrap();
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
    fn collapsing_hides_the_child_rows_and_expanding_shows_them() {
        let mut fixture = orchestrated();
        let toggle = format!("toggle-{}", fixture.orchestrator);
        let child = Id::from(format!("session-{}", fixture.child));

        let messages = click(&fixture.state, toggle.clone());
        assert_eq!(
            messages,
            [crate::app::Message::Projects(ProjectsMessage::Toggle(
                fixture.orchestrator.clone()
            ))]
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
    fn collapsing_a_row_above_the_cursor_moves_the_cursor_onto_it() {
        let mut fixture = orchestrated();
        fixture.state.projects.cursor = Some(fixture.child.clone());

        apply(
            &mut fixture.state,
            [ProjectsMessage::Toggle(fixture.orchestrator.clone())],
        );
        assert_eq!(
            fixture.state.projects.cursor,
            Some(fixture.orchestrator.clone())
        );

        apply(
            &mut fixture.state,
            [ProjectsMessage::Toggle(fixture.orchestrator.clone())],
        );
        assert_eq!(fixture.state.projects.cursor, Some(fixture.orchestrator));
    }

    #[test]
    fn selecting_a_session_expands_the_collapsed_sessions_above_it() {
        let mut fixture = orchestrated();
        let project = fixture.state.app.tree().projects[0].id.clone();
        let other = fixture
            .state
            .app
            .new_session(project, Scope::Project)
            .unwrap();
        apply(
            &mut fixture.state,
            [
                ProjectsMessage::Toggle(fixture.orchestrator.clone()),
                // Collapsed, but not above the child: it stays as it is.
                ProjectsMessage::Toggle(other.clone()),
                ProjectsMessage::Toggle(fixture.child.clone()),
            ],
        );
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
        assert_eq!(
            fixture.state.projects.collapsed,
            HashSet::from([other, fixture.child.clone()])
        );
        assert_eq!(fixture.state.projects.cursor, Some(fixture.child.clone()));
        fixture.state.tab = Tab::Projects;
        let mut ui = simulator(view(&fixture.state));
        ui.find(child_row).unwrap();
    }

    #[test]
    fn the_sessions_above_a_session_are_found_outermost_first() {
        let fixture = orchestrated();
        let tree = fixture.state.app.tree();
        let sessions = &tree.projects[0].sessions;
        let above_of = |target: &SessionId| {
            let mut above = Vec::new();
            let found = sessions_above(sessions, target, &mut above);
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
    fn rows_sit_at_their_level_right_of_the_project_header() {
        let dir = project_dir();
        let (state, _backend, [first, _]) = state_with_project(&dir);
        let project = state.app.tree().projects[0].id.clone();

        let mut ui = simulator(view(&state));
        let header = bounds_of(&mut ui, format!("project-{project}"));
        let session = bounds_of(&mut ui, format!("session-{first}"));
        let new_session = bounds_of(&mut ui, format!("new-session-{project}"));
        let in_api = bounds_of(&mut ui, format!("new-session-{project}-api"));
        let open_folder = bounds_of(&mut ui, "open-project-folder".to_owned());
        // A row's content starts with its slot, which reaches left of its icon.
        let top_level = header.x + SESSION_INDENT - SLOT_REACH;
        assert!((header.x - TREE_MARGIN_X).abs() < 0.5);
        assert!((session.x - top_level).abs() < 0.5);
        assert!((new_session.x - top_level).abs() < 0.5);
        assert!((in_api.x - top_level - LEVEL_INDENT).abs() < 0.5);
        assert!((open_folder.x - (header.x - SLOT_REACH)).abs() < 0.5);
        assert!((session.height - ROW_HEIGHT).abs() < 0.5);
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
            backends,
            default,
        )
        .unwrap();
        let project = app.open_project(dir.path()).unwrap();
        let session = app.new_session(project, Scope::Project).unwrap();

        let state = boot(app, mpsc::unbounded().1).0;

        assert_eq!(state.projects.cursor, Some(session));
        assert!(state.projects.collapsed.is_empty());
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
            cursor_after(ProjectsMessage::Toggle(orchestrator.clone())),
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
        assert_eq!(session_counts(state), [2, 1]);
        assert_eq!(state.tab, Tab::Session);
        let created = state.app.current_session().unwrap();
        assert_eq!(state.projects.cursor, Some(created));

        state.tab = Tab::Projects;
        state.projects.cursor = Some(other);
        apply(state, [ProjectsMessage::NewSessionAtCursor]);
        assert_eq!(session_counts(state), [2, 2]);
        let tree = state.app.tree();
        let newest = &tree.projects[1].sessions[0];
        assert_eq!(Some(newest.id.clone()), state.app.current_session());
        assert_eq!(newest.scope_label, "");

        // A cursor on a spawned session still names its project.
        state.tab = Tab::Projects;
        state.projects.cursor = Some(fixture.child.clone());
        apply(state, [ProjectsMessage::NewSessionAtCursor]);
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
    fn new_session_for_a_repository_creates_a_repository_session() {
        let dir = project_dir();
        let (mut state, backend, _) = state_with_project(&dir);
        let project = state.app.tree().projects[0].clone();
        let repository = project.repositories[0].clone();

        let messages = click(
            &state,
            format!("new-session-{}-{}", project.id, repository.id),
        );
        for message in messages {
            let _task = update(&mut state, message);
        }

        let calls = backend.calls();
        let (_, session, cwd) = calls.last().and_then(Call::as_new_session).unwrap();
        assert_eq!(cwd, dir.path().join("api"));
        assert_eq!(state.app.current_session().as_ref(), Some(session));
        assert_eq!(state.tab, Tab::Session);
        let tree = state.app.tree();
        let newest = &tree.projects[0].sessions[0];
        assert_eq!(newest.scope_label, "api");
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
        }
    }

    #[test]
    fn a_waiting_session_shows_the_asks_you_badge() {
        let dir = project_dir();
        let (mut state, backend, [_, second]) = state_with_project(&dir);
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

        ui.find("asks you").unwrap();
        assert!(ui.find("idle").is_err());
    }

    #[test]
    fn a_parent_of_a_waiting_child_is_marked_in_the_tree() {
        let mut fixture = orchestrated();
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
    fn a_session_with_children_tells_how_many_are_done() {
        let mut fixture = orchestrated();
        let (orchestrator, child) = (fixture.orchestrator.clone(), fixture.child.clone());
        {
            let mut ui = simulator(view(&fixture.state));
            let row = bounds_of(&mut ui, format!("session-{orchestrator}"));
            assert!(shows_within(&mut ui, "0 of 1 children done", row));
        }

        deliver(&mut fixture.state, turn_ended(&child));

        let mut ui = simulator(view(&fixture.state));
        let row = bounds_of(&mut ui, format!("session-{orchestrator}"));
        assert!(shows_within(&mut ui, "1 of 1 children done", row));
        assert!(ui.find("0 of 1 children done").is_err());
    }

    #[test]
    fn a_session_without_children_shows_its_context_usage_once_known() {
        let mut fixture = orchestrated();
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

        // The orchestrator's row tells about its children instead.
        let mut ui = simulator(view(&fixture.state));
        let row = bounds_of(&mut ui, format!("session-{child}"));
        assert!(shows_within(&mut ui, "41k / 200k", row));
        let orchestrator = bounds_of(&mut ui, format!("session-{}", fixture.orchestrator));
        assert!(!shows_within(&mut ui, "41k / 200k", orchestrator));
        assert!(shows_within(&mut ui, "0 of 1 children done", orchestrator));
    }

    #[test]
    fn the_status_line_counts_the_sessions_by_status_at_any_depth() {
        let dir = project_dir();
        let (mut fixture, _idle) = orchestrated_and_another_project(&dir);
        let (orchestrator, child) = (fixture.orchestrator.clone(), fixture.child.clone());
        {
            let mut ui = simulator(view(&fixture.state));
            ui.find("2 running").unwrap();
            for absent in ["asks you", "failed", "done", "idle"] {
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
        let mut ui = simulator(view(&state));
        ui.find("No projects open").unwrap();
    }

    #[test]
    fn closing_a_project_forgets_its_collapsed_rows_and_its_cursor() {
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
        apply(
            &mut fixture.state,
            [
                ProjectsMessage::Toggle(fixture.orchestrator.clone()),
                ProjectsMessage::Toggle(other.clone()),
            ],
        );
        fixture.state.projects.cursor = Some(other.clone());

        // Closing the first project keeps what belongs to the second.
        apply(
            &mut fixture.state,
            [ProjectsMessage::Close(projects[0].clone())],
        );
        assert_eq!(
            fixture.state.projects.collapsed,
            HashSet::from([other.clone()])
        );
        assert_eq!(fixture.state.projects.cursor, Some(other));

        apply(
            &mut fixture.state,
            [ProjectsMessage::Close(projects[1].clone())],
        );
        assert!(fixture.state.projects.collapsed.is_empty());
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
        }

        apply(&mut fixture.state, [ProjectsMessage::Close(first)]);

        // What belongs to the other project's session stays.
        let kept = HashSet::from([entry(&other)]);
        assert_eq!(fixture.state.session.expanded, kept);
        assert_eq!(fixture.state.session.expanded_tools, kept);
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
            backends,
            default,
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
            backends,
            default,
        )
        .unwrap();
        boot(app, mpsc::unbounded().1).0
    }

    #[test]
    fn a_project_restored_at_startup_lists_its_sessions() {
        let dir = project_dir();
        let store = MemoryStore::new();
        let (parent, child) = stored_parent_and_child(&dir, &store);

        let state = restarted(store);
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

        let state = restarted(store);
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
            ui.find(Id::from(format!("restore-{first}"))).unwrap();
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
        ui.find(Id::from(format!("restore-{orchestrator}")))
            .unwrap();
        assert!(ui.find(Id::from(format!("restore-{child}"))).is_err());
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

        let messages = click(&state, format!("restore-{first}"));
        assert_eq!(
            messages,
            [crate::app::Message::Projects(ProjectsMessage::Restore(
                first.clone()
            ))]
        );
        apply(&mut state, [ProjectsMessage::Restore(first.clone())]);

        assert_eq!(state.error, None);
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
        assert!(strip.x > row.center_x());
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
    fn the_status_line_names_the_delete_key() {
        let (state, _backend) = empty_state();
        let mut ui = simulator(view(&state));

        ui.find("D").unwrap();
        ui.find("delete").unwrap();
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
    fn an_archived_row_offers_a_delete_link_beside_restore() {
        let dir = project_dir();
        let (mut state, _backend, [first, _]) = state_with_project(&dir);
        let project = one_archived(&mut state, &first);
        apply(&mut state, [ProjectsMessage::ToggleArchived(project)]);
        {
            let mut ui = simulator(view(&state));
            let row = bounds_of(&mut ui, format!("session-{first}"));
            let restore = bounds_of(&mut ui, format!("restore-{first}"));
            let delete = bounds_of(&mut ui, format!("delete-{first}"));
            assert!(row.contains(delete.center()));
            assert!(delete.x > restore.x);
            assert!(shows_within(&mut ui, "delete", delete));
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
    fn an_archived_child_has_no_delete_link() {
        let mut fixture = orchestrated();
        fixture
            .state
            .app
            .archive(fixture.orchestrator.clone())
            .unwrap();
        let project = fixture.state.app.tree().projects[0].id.clone();
        apply(
            &mut fixture.state,
            [ProjectsMessage::ToggleArchived(project)],
        );
        let mut ui = simulator(view(&fixture.state));

        ui.find(Id::from(format!("delete-{}", fixture.orchestrator)))
            .unwrap();
        assert!(
            ui.find(Id::from(format!("delete-{}", fixture.child)))
                .is_err()
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
            fixture.state.projects.collapsed.insert(session.clone());
        }
        fixture.state.projects.hovered = Some(fixture.child.clone());

        delete_confirmed(&mut fixture.state, fixture.orchestrator.clone());

        assert_eq!(fixture.state.error, None);
        let kept = HashSet::from([entry(&other)]);
        assert_eq!(fixture.state.session.expanded, kept);
        assert_eq!(fixture.state.session.expanded_tools, kept);
        assert_eq!(fixture.state.projects.collapsed, HashSet::from([other]));
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

        assert!(!delete(&mut state, SessionId::new()));

        assert!(state.error.is_some());
        assert_eq!(state.tab, Tab::Session);
        assert_eq!(state.projects.cursor, Some(first.clone()));
        assert_eq!(state.projects.hovered, Some(first));
        assert_eq!(state.app.tree().projects[0].sessions.len(), 2);
    }
}
