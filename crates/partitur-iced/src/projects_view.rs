//! The Projects tab: the open projects with their sessions and repositories.
//!
//! One centered column. Each project has a header (name, folder, a chip when the folder has
//! an `AGENTS.md`, and a close button), its sessions as rows indented by how deeply they
//! were spawned from other sessions, a "new session" row for the project and one for each of
//! its repositories. A last row opens another project folder.

use std::path::PathBuf;

use iced::font::Weight;
use iced::widget::{Column, button, container, row, scrollable, space, text};
use iced::{Alignment, Border, Element, Font, Length, Padding, Task, Theme};
use partitur_core::{
    ProjectId, ProjectNode, RepositoryNode, Scope, SessionId, SessionNode, Status, TreeModel,
};

use crate::app::{Message, State, Tab, report};
use crate::{layout, session_view};

/// How far each level of spawned sessions is indented, in logical pixels.
const INDENT: f32 = 20.0;

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
    for project in &tree.projects {
        content = content
            .push(project_header(project))
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
/// answers and expanded entries of its sessions.
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

/// The project's name, folder and chip, with the close button on the right.
fn project_header<'a>(project: &ProjectNode) -> Element<'a, Message> {
    let mut header = row![
        text(project.name.clone()).font(bold()).size(18),
        text(project.root.display().to_string()).style(dimmed),
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    if project.has_global_instructions {
        header = header.push(chip("global AGENTS.md"));
    }
    let close = button(text("×"))
        .padding([2, 10])
        .style(row_style)
        .on_press(Message::Projects(ProjectsMessage::Close(
            project.id.clone(),
        )));
    let close = container(close).id(format!("close-{}", project.id));
    container(row![header, space::horizontal(), close].align_y(Alignment::Center))
        .padding(Padding::ZERO.top(12))
        .into()
}

/// The project's sessions, then its "new session" rows.
fn project_body<'a>(project: &ProjectNode) -> Element<'a, Message> {
    let mut body = Column::new().spacing(2);
    for session in &project.sessions {
        body = push_session(body, session, 0);
    }
    body = body.push(new_session_row(
        project,
        Scope::Project,
        format!("new-session-{}", project.id),
        row![text("+ new session")],
    ));
    for repository in &project.repositories {
        body = body.push(repository_row(project, repository));
    }
    body.into()
}

/// Adds `session` at `depth`, followed by the sessions spawned from it one level deeper.
fn push_session<'a>(
    mut body: Column<'a, Message>,
    session: &SessionNode,
    depth: u16,
) -> Column<'a, Message> {
    let line = row![
        text(session.title.clone()).width(Length::Fill),
        status_badge(session.status),
        text(session.scope_label.clone()).style(dimmed),
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    let select = button(line)
        .width(Length::Fill)
        .style(row_style)
        .on_press(Message::Projects(ProjectsMessage::Select(
            session.id.clone(),
        )));
    body = body.push(
        container(container(select).id(format!("session-{}", session.id)))
            .padding(Padding::ZERO.left(indent(depth))),
    );
    for child in &session.children {
        body = push_session(body, child, depth.saturating_add(1));
    }
    body
}

/// The row that starts a session for `repository`: its name and an `AGENTS.md` chip.
fn repository_row<'a>(project: &ProjectNode, repository: &RepositoryNode) -> Element<'a, Message> {
    let mut label = row![
        text("+ new session"),
        text(repository.name.clone()).font(bold())
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if repository.has_instructions {
        label = label.push(chip("AGENTS.md"));
    }
    new_session_row(
        project,
        Scope::Repository(repository.id.clone()),
        format!("new-session-{}-{}", project.id, repository.id),
        label,
    )
}

/// A button over `label` that starts a session of `scope` in `project`, found by `id`.
fn new_session_row<'a>(
    project: &ProjectNode,
    scope: Scope,
    id: String,
    label: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    let start = button(label)
        .width(Length::Fill)
        .style(row_style)
        .on_press(Message::Projects(ProjectsMessage::NewSession(
            project.id.clone(),
            scope,
        )));
    container(start).id(id).into()
}

/// The row at the bottom that opens the folder dialog.
fn open_folder_button<'a>() -> Element<'a, Message> {
    let open = button(text("+ open project folder"))
        .style(row_style)
        .on_press(Message::Projects(ProjectsMessage::OpenFolder));
    container(open).padding(Padding::ZERO.top(16)).into()
}

/// The pill that tells where a session stands.
///
/// Running is green, "asks you" amber and failed red; idle and done are grey.
pub(crate) fn status_badge<'a>(status: Status) -> Element<'a, Message> {
    let label = match status {
        Status::Idle => "idle",
        Status::Running => "running",
        Status::WaitingForYou => "asks you",
        Status::Done => "done",
        Status::Failed => "failed",
    };
    container(
        text(label)
            .size(12)
            .style(move |theme: &Theme| text::Style {
                color: Some(status_color(theme, status)),
            }),
    )
    .padding([1, 8])
    .style(pill_style)
    .into()
}

/// A small pill with `label`, for facts such as which `AGENTS.md` files exist.
pub(crate) fn chip<'a>(label: &'static str) -> Element<'a, Message> {
    container(text(label).size(12).style(dimmed))
        .padding([1, 8])
        .style(pill_style)
        .into()
}

/// The color a status badge's text takes.
fn status_color(theme: &Theme, status: Status) -> iced::Color {
    let palette = theme.extended_palette();
    match status {
        Status::Idle | Status::Done => palette.background.weak.text.scale_alpha(0.7),
        Status::Running => palette.success.base.color,
        Status::WaitingForYou => palette.warning.base.color,
        Status::Failed => palette.danger.base.color,
    }
}

/// The indentation of a session `depth` levels below the project's top-level sessions.
fn indent(depth: u16) -> f32 {
    f32::from(depth) * INDENT
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
    use partitur_core::testing::{Call, MemoryStore, RecordingBackend};
    use partitur_core::{
        App, BackendEvent, PermissionId, PermissionKind, PermissionOption, PermissionOptionId,
        Scope, SessionId, ToolCallId, ToolKind, Update,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::app::{State, Tab, boot, update, view};

    /// A project folder with the repository `api`, both with an `AGENTS.md`.
    fn project_dir() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("api").join(".git")).unwrap();
        fs::write(dir.path().join("AGENTS.md"), "global").unwrap();
        fs::write(dir.path().join("api").join("AGENTS.md"), "api only").unwrap();
        dir
    }

    /// A state with the project of `dir` open (the way the folder picker opens it) and two
    /// project sessions, oldest first, plus the backend that recorded them.
    fn state_with_project(dir: &TempDir) -> (State, RecordingBackend, [SessionId; 2]) {
        let backend = RecordingBackend::new();
        let app = App::load(Box::new(backend.clone()), Box::new(MemoryStore::new())).unwrap();
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
        ui.find("api").unwrap();
        ui.find("AGENTS.md").unwrap();
        ui.find("idle").unwrap();
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
    fn without_projects_the_tab_offers_to_open_one() {
        let app = App::load(
            Box::new(RecordingBackend::new()),
            Box::new(MemoryStore::new()),
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
                update: Update::PermissionRequested {
                    id: PermissionId(1),
                    tool_call_id: ToolCallId::new("t-1"),
                    title: "rm -rf".into(),
                    kind: ToolKind::Execute,
                    options: vec![PermissionOption {
                        id: PermissionOptionId::new("allow"),
                        name: "Allow".into(),
                        kind: PermissionKind::AllowOnce,
                    }],
                },
            })
            .unwrap();

        let mut ui = simulator(view(&state));

        ui.find("asks you").unwrap();
        ui.find("idle").unwrap();
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
        let mut app =
            App::load(Box::new(RecordingBackend::new()), Box::new(store.clone())).unwrap();
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

        let app = App::load(Box::new(RecordingBackend::new()), Box::new(store)).unwrap();
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

        let app = App::load(Box::new(RecordingBackend::new()), Box::new(store)).unwrap();
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
