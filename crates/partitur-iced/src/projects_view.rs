//! The Projects tab: the open projects with their sessions and repositories.
//!
//! One centered column of project blocks. A block starts with the project's header: its name,
//! its folder, a chip for each of `AGENTS.md` and `CLAUDE.md` the folder has, and a close
//! button on the right.
//! Under it, one step to the right, come the sessions as rows: a chevron when sessions were
//! spawned from the session, the title, a status pill (none while idle), a chip when a
//! session below waits for the user, then the repository and backend dimmed. Spawned sessions
//! sit one step deeper than their parent. The block ends with a dimmed `+ new session` row at
//! the sessions' step and, one step deeper, a `+ in <repository>` row for every repository
//! other than the project root. A last row opens another project folder.

use std::path::PathBuf;

use iced::font::Weight;
use iced::theme::palette::Pair;
use iced::widget::{Column, button, container, row, scrollable, space, text};
use iced::{Alignment, Border, Element, Font, Length, Padding, Task, Theme};
use partitur_core::{
    ProjectId, ProjectNode, RepositoryNode, Scope, SessionId, SessionNode, Status, TreeModel,
};

use crate::app::{Message, State, Tab, report};
use crate::{layout, session_view};

/// How far one step of indentation moves a row to the right, in logical pixels: the
/// sessions sit one step right of the project header, each level of spawned sessions one
/// step right of its parent.
const INDENT: f32 = 20.0;

/// The width of the chevron slot at the left of a session row, in logical pixels.
const GLYPH_WIDTH: f32 = 12.0;

/// The vertical space between one project's block and the next, in logical pixels.
const PROJECT_GAP: f32 = 14.0;

/// The chip that marks a session with a session below it that waits for the user.
pub(crate) const CHILD_ASKS_YOU: &str = "child asks you";

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
}

/// The Projects tab: every project of `tree` with its sessions and repositories.
///
/// The element owns what it shows, so it does not borrow `tree`, which the caller builds
/// afresh for every frame.
pub fn view<'a>(tree: &TreeModel) -> Element<'a, Message> {
    let mut content = Column::new()
        .spacing(4)
        .padding(Padding::ZERO.top(16).bottom(16));
    if tree.projects.is_empty() {
        content = content.push(text("No projects open").style(dimmed));
    }
    for (index, project) in tree.projects.iter().enumerate() {
        let gap = if index == 0 { 0.0 } else { PROJECT_GAP };
        content = content
            .push(project_header(project, gap))
            .push(project_body(project));
    }
    content = content.push(open_folder_button());
    layout::centered(scrollable(content).width(Length::Fill).into())
}

/// Applies `message` to the Projects tab and returns the follow-up work.
///
/// Opening, creating, selecting and closing go through the core [`App`](partitur_core::App);
/// when it refuses, the error bar shows why. Creating or selecting a session switches to
/// the Session tab, where the session is now current; closing a project forgets the parsed
/// answers, expanded entries and approval card drafts of its sessions.
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
                state.tab = Tab::Session;
                session_view::sync_markdown(state);
            }
        }
        ProjectsMessage::Select(session) => {
            if report(&mut state.error, state.app.select_session(session)).is_some() {
                state.tab = Tab::Session;
                session_view::sync_markdown(state);
            }
        }
        ProjectsMessage::Close(project) => {
            let closing = project_sessions(&state.app.tree(), &project);
            if report(&mut state.error, state.app.close_project(project)).is_some() {
                state
                    .session
                    .expanded
                    .retain(|(session, _)| !closing.contains(session));
                state
                    .session
                    .proposals
                    .retain(|(session, _), _| !closing.contains(session));
                for session in closing {
                    state.session.markdown.forget(&session);
                }
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

/// The sessions of `project` in `tree`, spawned ones included.
fn project_sessions(tree: &TreeModel, project: &ProjectId) -> Vec<SessionId> {
    fn collect(sessions: &[SessionNode], ids: &mut Vec<SessionId>) {
        for session in sessions {
            ids.push(session.id.clone());
            collect(&session.children, ids);
        }
    }
    let mut ids = Vec::new();
    if let Some(project) = tree.projects.iter().find(|open| &open.id == project) {
        collect(&project.sessions, &mut ids);
    }
    ids
}

/// The project's name, folder and chip, with the close button on the right, found by
/// `project-<id>`; `gap` is the space above it that separates it from the project before.
fn project_header<'a>(project: &ProjectNode, gap: f32) -> Element<'a, Message> {
    let mut header = row![
        text(project.name.clone()).font(bold()).size(17),
        text(project.root.display().to_string())
            .size(13)
            .style(dimmed),
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    if project.has_global_instructions {
        header = header.push(chip("global AGENTS.md"));
    }
    if project.has_global_claude_md {
        header = header.push(chip("global CLAUDE.md"));
    }
    let close = button(text("×"))
        .padding([2, 10])
        .style(row_style)
        .on_press(Message::Projects(ProjectsMessage::Close(
            project.id.clone(),
        )));
    let close = container(close).id(format!("close-{}", project.id));
    container(row![header, space::horizontal(), close].align_y(Alignment::Center))
        .id(format!("project-{}", project.id))
        .padding(Padding::ZERO.top(gap))
        .into()
}

/// The project's sessions one step right of its header, then the `+ new session` row at the
/// same step and a row per repository one step deeper.
fn project_body<'a>(project: &ProjectNode) -> Element<'a, Message> {
    let mut body = Column::new().spacing(2);
    for session in &project.sessions {
        body = push_session(body, session, 1);
    }
    body = body.push(new_session_row(
        project,
        Scope::Project,
        format!("new-session-{}", project.id),
        text("+ new session").style(dimmed),
        1,
    ));
    for repository in &project.repositories {
        body = body.push(repository_row(project, repository));
    }
    body.into()
}

/// Adds `session` `depth` steps right of the project header: a chevron when sessions were
/// spawned from it, its title, its status pill, a [`CHILD_ASKS_YOU`] chip when a session
/// below it needs the user, and its repository and backend dimmed. The sessions spawned
/// from it follow one step deeper.
fn push_session<'a>(
    mut body: Column<'a, Message>,
    session: &SessionNode,
    depth: u16,
) -> Column<'a, Message> {
    let glyph = if session.children.is_empty() {
        ""
    } else {
        "▾"
    };
    let mut line = row![text(glyph).width(GLYPH_WIDTH), text(session.title.clone())]
        .spacing(8)
        .align_y(Alignment::Center);
    if let Some(badge) = status_badge(session.status) {
        line = line.push(badge);
    }
    if session.attention && session.status != Status::WaitingForYou {
        line = line.push(chip(CHILD_ASKS_YOU));
    }
    line = line.push(text(meta(session)).size(13).style(dimmed));
    let select = button(line)
        .width(Length::Fill)
        .style(row_style)
        .on_press(Message::Projects(ProjectsMessage::Select(
            session.id.clone(),
        )));
    body = body.push(indented(
        container(select).id(format!("session-{}", session.id)),
        depth,
    ));
    for child in &session.children {
        body = push_session(body, child, depth.saturating_add(1));
    }
    body
}

/// The text after a session's title: its repository and backend, or only the backend for a
/// session of the whole project.
fn meta(session: &SessionNode) -> String {
    if session.scope_label.is_empty() {
        format!("· {}", session.backend)
    } else {
        format!("{} · {}", session.scope_label, session.backend)
    }
}

/// The row that starts a session in `repository`, one step deeper than the sessions: `+ in`
/// and its name, with an `AGENTS.md` chip and a `CLAUDE.md` chip for the files it has.
fn repository_row<'a>(project: &ProjectNode, repository: &RepositoryNode) -> Element<'a, Message> {
    let mut label = row![
        text(format!("+ in {}", repository.name))
            .size(13)
            .style(dimmed)
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if repository.has_instructions {
        label = label.push(chip("AGENTS.md"));
    }
    if repository.has_claude_md {
        label = label.push(chip("CLAUDE.md"));
    }
    new_session_row(
        project,
        Scope::Repository(repository.id.clone()),
        format!("new-session-{}-{}", project.id, repository.id),
        label,
        2,
    )
}

/// A button over `label`, `depth` steps right of the project header, that starts a session
/// of `scope` in `project`, found by `id`.
fn new_session_row<'a>(
    project: &ProjectNode,
    scope: Scope,
    id: String,
    label: impl Into<Element<'a, Message>>,
    depth: u16,
) -> Element<'a, Message> {
    let start = button(label)
        .width(Length::Fill)
        .style(row_style)
        .on_press(Message::Projects(ProjectsMessage::NewSession(
            project.id.clone(),
            scope,
        )));
    indented(container(start).id(id), depth)
}

/// `content` moved `depth` steps right of the project header's left edge.
fn indented<'a>(content: impl Into<Element<'a, Message>>, depth: u16) -> Element<'a, Message> {
    container(content)
        .padding(Padding::ZERO.left(f32::from(depth) * INDENT))
        .into()
}

/// The row at the bottom that opens the folder dialog.
fn open_folder_button<'a>() -> Element<'a, Message> {
    let open = button(text("+ open project folder").style(dimmed))
        .style(row_style)
        .on_press(Message::Projects(ProjectsMessage::OpenFolder));
    container(open).padding(Padding::ZERO.top(18)).into()
}

/// The pill that tells where a session stands, or `None` while it is idle, which needs no
/// telling.
///
/// Running is tinted green, "asks you" amber, failed red and done neutral.
pub(crate) fn status_badge<'a>(status: Status) -> Option<Element<'a, Message>> {
    let label = match status {
        Status::Idle => return None,
        Status::Running => "running",
        Status::WaitingForYou => "asks you",
        Status::Done => "done",
        Status::Failed => "failed",
    };
    let pill = container(text(label).size(12))
        .padding([1, 8])
        .style(move |theme: &Theme| {
            let tint = status_tint(theme, status);
            container::Style {
                background: Some(tint.color.into()),
                text_color: Some(tint.text),
                border: Border::default().rounded(10),
                ..container::Style::default()
            }
        });
    Some(pill.into())
}

/// A small pill with `label`, for facts such as which instruction files exist.
pub(crate) fn chip<'a>(label: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    container(text(label).size(12).style(dimmed))
        .padding([1, 8])
        .style(pill_style)
        .into()
}

/// The tint of a status badge: the pill's background and the text that reads on it.
fn status_tint(theme: &Theme, status: Status) -> Pair {
    let palette = theme.extended_palette();
    match status {
        Status::Idle | Status::Done => palette.background.weak,
        Status::Running => palette.success.weak,
        Status::WaitingForYou => palette.warning.weak,
        Status::Failed => palette.danger.weak,
    }
}

/// The default font in bold.
pub(crate) fn bold() -> Font {
    Font {
        weight: Weight::Bold,
        ..Font::DEFAULT
    }
}

/// Secondary text: the theme's text color, faded.
pub(crate) fn dimmed(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(
            theme
                .extended_palette()
                .background
                .base
                .text
                .scale_alpha(0.6),
        ),
    }
}

/// The background of badges and chips.
pub(crate) fn pill_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(theme.extended_palette().background.weak.color.into()),
        border: Border::default().rounded(10),
        ..container::Style::default()
    }
}

/// A row button: no chrome until hovered, then the weak background.
pub(crate) fn row_style(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: hovered.then(|| palette.background.weak.color.into()),
        text_color: palette.background.base.text,
        border: Border::default().rounded(6),
        ..button::Style::default()
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

    use std::fs;

    use futures::channel::mpsc;
    use iced::widget::Id;
    use iced_test::simulator;
    use partitur_core::Store;
    use partitur_core::testing::{Call, MemoryStore, RecordingBackend, single_claude};
    use partitur_core::{
        App, BackendEvent, PermissionId, PermissionKind, PermissionOption, PermissionOptionId,
        ProjectId, RepositoryId, Scope, SessionId, StopReason, ToolCallId, ToolKind, Update,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::app::{State, Tab, boot, update, view};
    use crate::test_support::{deliver, orchestrated, two_backends};

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

    /// A state with the project of `dir` open on the two configured backends of
    /// [`two_backends`], and one session of `scope` in it.
    fn state_on_two_backends(dir: &TempDir, scope: Scope) -> (State, ProjectId, SessionId) {
        let (backends, default) = two_backends();
        let app = App::load(
            Box::new(RecordingBackend::new()),
            Box::new(MemoryStore::new()),
            backends,
            default,
        )
        .unwrap();
        let mut state = boot(app, mpsc::unbounded().1).0;
        let project = state.app.open_project(dir.path()).unwrap();
        let session = state.app.new_session(project.clone(), scope).unwrap();
        (state, project, session)
    }

    /// A state with the project of `dir` open (the way the folder picker opens it) and two
    /// project sessions, oldest first, plus the backend that recorded them.
    fn state_with_project(dir: &TempDir) -> (State, RecordingBackend, [SessionId; 2]) {
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

    #[test]
    fn an_open_project_lists_its_sessions_and_repositories() {
        let dir = project_dir();
        let (state, _backend, [first, second]) = state_with_project(&dir);
        let project = state.app.tree().projects[0].clone();
        let mut ui = simulator(view(&state));

        ui.find(project.name.as_str()).unwrap();
        ui.find("global AGENTS.md").unwrap();
        ui.find("+ new session").unwrap();
        ui.find("+ in api").unwrap();
        ui.find("AGENTS.md").unwrap();
        ui.find(Id::from(format!("session-{first}"))).unwrap();
        ui.find(Id::from(format!("session-{second}"))).unwrap();
        ui.find(Id::from(format!("new-session-{}", project.id)))
            .unwrap();
        ui.find(Id::from(format!(
            "new-session-{}-{}",
            project.id, project.repositories[0].id
        )))
        .unwrap();
        ui.find("+ open project folder").unwrap();
        assert!(ui.find("No projects open").is_err());
    }

    #[test]
    fn a_root_claude_md_shows_a_global_chip() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("api").join(".git")).unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "global").unwrap();
        let (state, _backend, _) = state_with_project(&dir);

        let mut ui = simulator(view(&state));
        ui.find("global CLAUDE.md").unwrap();
        assert!(ui.find("CLAUDE.md").is_err());
        assert!(ui.find("global AGENTS.md").is_err());
    }

    #[test]
    fn a_repository_claude_md_shows_a_chip_on_its_row() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("api").join(".git")).unwrap();
        fs::write(dir.path().join("api").join("CLAUDE.md"), "api only").unwrap();
        let (state, _backend, _) = state_with_project(&dir);

        let mut ui = simulator(view(&state));
        ui.find("+ in api").unwrap();
        ui.find("CLAUDE.md").unwrap();
        assert!(ui.find("global CLAUDE.md").is_err());
        assert!(ui.find("AGENTS.md").is_err());
    }

    #[test]
    fn a_project_with_both_instruction_files_shows_both_chips() {
        let dir = project_dir();
        fs::write(dir.path().join("CLAUDE.md"), "global").unwrap();
        fs::write(dir.path().join("api").join("CLAUDE.md"), "api only").unwrap();
        let (state, _backend, _) = state_with_project(&dir);

        let mut ui = simulator(view(&state));
        ui.find("global AGENTS.md").unwrap();
        ui.find("global CLAUDE.md").unwrap();
        ui.find("AGENTS.md").unwrap();
        ui.find("CLAUDE.md").unwrap();
    }

    #[test]
    fn a_session_row_shows_its_backend() {
        let dir = project_dir();
        let (state, _, session) = state_on_two_backends(&dir, Scope::Project);

        let mut ui = simulator(view(&state));
        ui.find(Id::from(format!("session-{session}"))).unwrap();
        ui.find("· claude-main").unwrap();
        assert!(ui.find("· claude-work").is_err());
    }

    #[test]
    fn a_repository_session_names_its_repository_before_the_backend() {
        let dir = project_dir();
        let api = RepositoryId::try_from("api").unwrap();
        let (state, _, session) = state_on_two_backends(&dir, Scope::Repository(api));

        let mut ui = simulator(view(&state));
        ui.find(Id::from(format!("session-{session}"))).unwrap();
        ui.find("api · claude-main").unwrap();
        assert!(ui.find("· claude-main").is_err());
    }

    #[test]
    fn an_idle_session_shows_no_status_badge() {
        let dir = project_dir();
        let (state, _backend, [first, _]) = state_with_project(&dir);

        let mut ui = simulator(view(&state));
        ui.find(Id::from(format!("session-{first}"))).unwrap();
        assert!(ui.find("idle").is_err());
        assert!(ui.find("▾").is_err());
    }

    #[test]
    fn a_project_that_is_itself_a_repository_offers_one_new_session_row() {
        let dir = root_repository_dir();
        let (state, project, _) = state_on_two_backends(&dir, Scope::Project);
        let root = dir.path().file_name().unwrap().to_string_lossy();

        let mut ui = simulator(view(&state));
        ui.find(Id::from(format!("new-session-{project}"))).unwrap();
        ui.find("+ new session").unwrap();
        assert!(
            ui.find(Id::from(format!("new-session-{project}-{root}")))
                .is_err()
        );
        assert!(ui.find(format!("+ in {root}").as_str()).is_err());
    }

    #[test]
    fn every_repository_gets_its_own_new_session_row() {
        let fixture = orchestrated();
        let project = fixture.state.app.tree().projects[0].id.clone();

        let mut ui = simulator(view(&fixture.state));
        ui.find("+ new session").unwrap();
        ui.find("+ in api").unwrap();
        ui.find("+ in web").unwrap();
        ui.find(Id::from(format!("new-session-{project}-api")))
            .unwrap();
        ui.find(Id::from(format!("new-session-{project}-web")))
            .unwrap();
    }

    #[test]
    fn a_session_with_children_carries_a_chevron() {
        let fixture = orchestrated();

        let mut ui = simulator(view(&fixture.state));
        ui.find("▾").unwrap();
    }

    #[test]
    fn session_rows_sit_one_step_right_of_the_project_header() {
        let dir = project_dir();
        let (state, _backend, [first, _]) = state_with_project(&dir);
        let project = state.app.tree().projects[0].id.clone();

        let mut ui = simulator(view(&state));
        let header = ui
            .find(Id::from(format!("project-{project}")))
            .unwrap()
            .bounds();
        let session = ui
            .find(Id::from(format!("session-{first}")))
            .unwrap()
            .bounds();
        let new_session = ui
            .find(Id::from(format!("new-session-{project}")))
            .unwrap()
            .bounds();
        let in_api = ui
            .find(Id::from(format!("new-session-{project}-api")))
            .unwrap()
            .bounds();
        assert!((session.x - header.x - INDENT).abs() < 0.5);
        assert!((new_session.x - header.x - INDENT).abs() < 0.5);
        assert!((in_api.x - header.x - 2.0 * INDENT).abs() < 0.5);
    }

    #[test]
    fn without_projects_the_tab_offers_to_open_one() {
        let (backends, default) = single_claude();
        let app = App::load(
            Box::new(RecordingBackend::new()),
            Box::new(MemoryStore::new()),
            backends,
            default,
        )
        .unwrap();
        let state = boot(app, mpsc::unbounded().1).0;
        let mut ui = simulator(view(&state));

        ui.find("No projects open").unwrap();
        ui.click("+ open project folder").unwrap();

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
            ui.find(Id::from(format!("session-{}", fixture.orchestrator)))
                .unwrap();
            ui.find("child asks you").unwrap();
            ui.find("asks you").unwrap();
            ui.find("running").unwrap();
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
        deliver(
            &mut fixture.state,
            BackendEvent::Update {
                session: fixture.orchestrator.clone(),
                update: Update::TurnEnded(StopReason::EndTurn),
            },
        );

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

    #[test]
    fn a_project_restored_at_startup_lists_its_sessions() {
        let dir = project_dir();
        let store = MemoryStore::new();
        let (parent, child) = stored_parent_and_child(&dir, &store);

        let (backends, default) = single_claude();
        let app = App::load(
            Box::new(RecordingBackend::new()),
            Box::new(store),
            backends,
            default,
        )
        .unwrap();
        let state = boot(app, mpsc::unbounded().1).0;
        let mut ui = simulator(view(&state));

        ui.find(Id::from(format!("session-{parent}"))).unwrap();
        ui.find(Id::from(format!("session-{child}"))).unwrap();
        ui.find("+ new session").unwrap();
    }

    #[test]
    fn a_spawned_session_is_indented_under_its_parent() {
        let dir = project_dir();
        let store = MemoryStore::new();
        let (parent, child) = stored_parent_and_child(&dir, &store);

        let (backends, default) = single_claude();
        let app = App::load(
            Box::new(RecordingBackend::new()),
            Box::new(store),
            backends,
            default,
        )
        .unwrap();
        let state = boot(app, mpsc::unbounded().1).0;
        let mut ui = simulator(view(&state));

        let parent = ui
            .find(Id::from(format!("session-{parent}")))
            .unwrap()
            .bounds();
        let child = ui
            .find(Id::from(format!("session-{child}")))
            .unwrap()
            .bounds();
        assert!((child.x - parent.x - INDENT).abs() < 0.5);
        assert!(child.y > parent.y);
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
}
