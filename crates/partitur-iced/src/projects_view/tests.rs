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
    AgentUpdate, App, BackendEvent, ExecutionMode, GlobalSettings, ModuleEvent, PermissionId,
    PermissionKind, PermissionOption, PermissionOptionId, PlannedTask, ProjectId, RepositoryId,
    Scope, SessionId, StopReason, SuperpowersV7Event, ToolCallId, ToolKind, TranscriptRecord,
};
use tempfile::TempDir;

use super::*;
use crate::app::{BareKey, Message, State, Tab, boot, update, view};
use crate::new_session_sheet::NewSessionMessage;
use crate::session_view::RowKey;
use crate::status_pills::CHILD_ASKS_YOU;
use crate::test_support::{
    Orchestrated, attach, deliver, deliver_tool, feed, orchestrated, orchestrated_after, shown,
    state_after_interrupted_turn, temp_dir,
};

/// A project folder with the repository `api`, both with an `AGENTS.md`.
fn project_dir() -> TempDir {
    let dir = temp_dir();
    fs::create_dir_all(dir.path().join("api").join(".git")).unwrap();
    fs::write(dir.path().join("AGENTS.md"), "global").unwrap();
    fs::write(dir.path().join("api").join("AGENTS.md"), "api only").unwrap();
    dir
}

/// A project folder that is itself a repository, so its only repository is its root.
fn root_repository_dir() -> TempDir {
    let dir = temp_dir();
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

/// Selects `session`, as a click on its row does, dropping the task.
fn select(state: &mut State, session: SessionId) {
    let _task = update(state, crate::app::Message::SelectSession(session));
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
fn shows_within(ui: &mut Simulator<'_, crate::app::Message>, label: &str, area: Rectangle) -> bool {
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
        update: AgentUpdate::TurnEnded(StopReason::EndTurn),
    }
}

#[test]
fn an_open_project_lists_its_sessions() {
    let dir = project_dir();
    let (state, _backend, [first, second]) = state_with_project(&dir);
    let project = state.app.tree().projects[0].clone();
    let folder = keep_end(
        &home_path::abbreviated(&project.root, std::env::home_dir().as_deref()),
        PATH_BUDGET,
    );
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
    let dir = temp_dir();
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
    let dir = temp_dir();
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
    let dir = temp_dir();
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
fn a_long_background_pill_fits_its_column_on_one_line() {
    let mut fixture = crate::test_support::plain();
    let session = fixture.session.clone();
    deliver(
        &mut fixture.state,
        BackendEvent::Update {
            session: session.clone(),
            update: AgentUpdate::RunningBackground(12),
        },
    );
    crate::test_support::end_turn(&mut fixture.state, &session);
    fixture.state.tab = Tab::Projects;

    let mut ui = simulator(view(&fixture.state));
    let row = bounds_of(&mut ui, format!("session-{session}"));
    let pill = ui
        .find(move |candidate: Candidate<'_>| match candidate {
            Candidate::Text {
                content, bounds, ..
            } if content == "12 background tasks" && row.contains(bounds.center()) => Some(bounds),
            _ => None,
        })
        .unwrap();
    // One line of the pill is 16 pixels high; a wrapped label is taller.
    assert!(pill.height < 20.0, "{pill:?}");
}

#[test]
fn a_session_whose_background_tasks_run_after_its_turn_shows_them_in_place_of_done() {
    let mut fixture = crate::test_support::plain();
    let session = fixture.session.clone();
    deliver(
        &mut fixture.state,
        BackendEvent::Update {
            session: session.clone(),
            update: AgentUpdate::RunningBackground(2),
        },
    );
    crate::test_support::end_turn(&mut fixture.state, &session);
    fixture.state.tab = Tab::Projects;

    let mut ui = simulator(view(&fixture.state));
    let row = bounds_of(&mut ui, format!("session-{session}"));
    assert!(shows_within(&mut ui, "2 background tasks", row));
    assert!(!shows_within(&mut ui, "done", row));
    drop(ui);

    deliver(
        &mut fixture.state,
        BackendEvent::Update {
            session: session.clone(),
            update: AgentUpdate::RunningBackground(0),
        },
    );
    let mut ui = simulator(view(&fixture.state));
    let row = bounds_of(&mut ui, format!("session-{session}"));
    assert!(shows_within(&mut ui, "done", row));
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
    select(&mut fixture.state, fixture.child.clone());

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
    select(&mut fixture.state, other.clone());

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

    select(&mut fixture.state, other);

    assert_eq!(fixture.state.app.tree().projects[1].fold, Fold::Open);
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
        [crate::app::Message::SelectSession(first.clone())]
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

    select(&mut state, first.clone());
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

    select(&mut state, SessionId::new());

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
fn asks_permission() -> AgentUpdate {
    AgentUpdate::PermissionRequested {
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
fn a_folded_parent_of_an_interrupted_child_is_marked_and_counted() {
    let mut fixture = orchestrated();
    let orchestrator = fixture.orchestrator.clone();
    deliver(&mut fixture.state, turn_ended(&orchestrator));
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
    let state = boot(reloaded, mpsc::unbounded().1, mpsc::unbounded().1).0;

    let mut ui = simulator(view(&state));
    let parent = bounds_of(&mut ui, format!("session-{orchestrator}"));
    assert!(ui.find(format!("session-{}", fixture.child)).is_err());
    assert!(shows_within(&mut ui, CHILD_ASKS_YOU, parent));
    assert!(shows_within(&mut ui, "idle", parent));
    ui.find("1 interrupted").unwrap();
}

#[test]
fn a_folded_parent_of_a_failed_child_is_marked_and_counted() {
    let mut fixture = orchestrated();
    let orchestrator = fixture.orchestrator.clone();
    deliver(
        &mut fixture.state,
        BackendEvent::Update {
            session: fixture.child.clone(),
            update: AgentUpdate::Failed("the agent crashed".into()),
        },
    );
    // The failed child's result starts another turn of the orchestrator, which ends too.
    deliver(&mut fixture.state, turn_ended(&orchestrator));
    deliver(&mut fixture.state, turn_ended(&orchestrator));

    let mut ui = simulator(view(&fixture.state));
    let parent = bounds_of(&mut ui, format!("session-{orchestrator}"));
    assert!(ui.find(format!("session-{}", fixture.child)).is_err());
    assert!(shows_within(&mut ui, CHILD_ASKS_YOU, parent));
    assert!(shows_within(&mut ui, "done", parent));
    ui.find("1 failed").unwrap();
}

#[test]
fn an_interrupted_session_does_not_mark_itself() {
    let dir = project_dir();
    let state = state_after_interrupted_turn(&dir);

    let mut ui = simulator(view(&state));
    assert_eq!(shown(&mut ui, "interrupted"), 1);
    assert_eq!(shown(&mut ui, CHILD_ASKS_YOU), 0);
    ui.find("1 interrupted").unwrap();
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
                update: AgentUpdate::Usage {
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
            update: AgentUpdate::Failed("the agent crashed".into()),
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
        SuperpowersV7Event::StartSession {
            topic: "Worktrees".into(),
        },
        SuperpowersV7Event::StartBrainstorming,
        SuperpowersV7Event::RequestDesignApproval {
            path: "docs/design.md".into(),
        },
    ] {
        deliver_tool(state, session.clone(), ModuleEvent::from(tool));
    }
    deliver(state, turn_ended(session));
}

/// Takes `session`, which waits at its design, through the plan to task 4 of 11.
fn executes_task_four(state: &mut State, session: &SessionId) {
    let tool = |state: &mut State, event| {
        deliver_tool(state, session.clone(), ModuleEvent::from(event));
    };
    state.app.approve_gate(session.clone()).unwrap();
    tool(state, SuperpowersV7Event::StartWritingPlan);
    tool(
        state,
        SuperpowersV7Event::RequestPlanApproval {
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
        SuperpowersV7Event::StartExecution {
            mode: ExecutionMode::Subagent,
        },
    );
    for n in 1..=3 {
        tool(state, SuperpowersV7Event::StartTask { n, note: None });
        tool(state, SuperpowersV7Event::FinishTask { n, note: None });
    }
    tool(state, SuperpowersV7Event::StartTask { n: 4, note: None });
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

    let other = temp_dir();
    let project = state.app.open_project(other.path()).unwrap();
    let second = state.app.new_session(project, Scope::Project).unwrap();
    waits_at_the_design(&mut state, &backend, &second);
    let mut ui = simulator(view(&state));
    ui.find("2 gates waiting").unwrap();
    ui.find("1 asks you").unwrap();
}

#[test]
fn a_child_at_its_gate_waits_for_its_orchestrator() {
    let dir = project_dir();
    let (mut fixture, other) = orchestrated_and_another_project(&dir);
    let (orchestrator, child) = (fixture.orchestrator.clone(), fixture.child.clone());
    crate::test_support::end_at_the_design(&mut fixture.state, &child);
    waits_at_the_design(&mut fixture.state, &fixture.backend, &other);
    assert!(fixture.state.app.waits_for_orchestrator(&child));

    // Folded, the orchestrator sums its child up as waiting for it.
    {
        let mut ui = simulator(view(&fixture.state));
        ui.find("1 child · api waits for orchestrator").unwrap();
        ui.find("1 gate waiting").unwrap();
        assert!(ui.find("1 asks you").is_err());
    }

    expand(&mut fixture);
    let mut ui = simulator(view(&fixture.state));
    let orchestrator_row = bounds_of(&mut ui, format!("session-{orchestrator}"));
    let child_row = bounds_of(&mut ui, format!("session-{child}"));
    assert!(shows_within(&mut ui, "waits for orchestrator", child_row));
    assert!(!shows_within(&mut ui, "asks you", child_row));
    assert!(!shows_within(&mut ui, CHILD_ASKS_YOU, orchestrator_row));
    // Only the other session's gate is the user's.
    ui.find("1 gate waiting").unwrap();
    assert!(ui.find("2 gates waiting").is_err());
    assert!(ui.find("1 asks you").is_err());
    drop(ui);

    // The child's dot is dim, the other session's amber.
    let dot_of = |session: &SessionId| {
        let node = node(&fixture.state, session);
        step_dot(node.step.as_ref().unwrap(), node.status)(&theme::LIGHT)
    };
    assert_eq!(dot_of(&child), theme::LIGHT.dim);
    assert_eq!(dot_of(&other), theme::LIGHT.asks_you.fg);

    // G goes to the other session's gate, not the child's.
    let _task = update(&mut fixture.state, Message::Key(BareKey::G));
    assert_eq!(fixture.state.app.current_session(), Some(other));
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
            session.role = partitur_core::Role::Child {
                parent: parent.clone(),
            };
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
        [crate::app::Message::SelectSession(first.clone())]
    );
    select(&mut state, first.clone());

    assert_eq!(state.tab, Tab::Session);
    assert_eq!(state.app.current_session(), Some(first.clone()));
    assert_eq!(state.projects.archived_open.len(), 1);
}

#[test]
fn selecting_an_archived_session_opens_its_group() {
    let dir = project_dir();
    let (mut state, _backend, [first, _]) = state_with_project(&dir);
    let project = one_archived(&mut state, &first);

    select(&mut state, first.clone());

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
        state.app.submit_prompt("go").unwrap();
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
        [crate::app::Message::SelectSession(fixture.child.clone())]
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
    let dir = temp_dir();
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
    state.app.submit_prompt("go").unwrap();
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
            update: AgentUpdate::Failed("the agent crashed".into()),
        },
    );
    let mut ui = simulator(view(&state));
    assert!(ui.find("1 idle").is_err());
    // A failed session needs you: it is counted with its dot, before the settled ones,
    // and marks no session but those above it.
    let done = ui.find("1 done").unwrap().bounds();
    let failed = ui.find("1 failed").unwrap().bounds();
    assert!(failed.x < done.x);
    assert_eq!(shown(&mut ui, CHILD_ASKS_YOU), 0);
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
        [crate::app::Message::SelectSession(fixture.child.clone())]
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
    let dir = temp_dir();
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
        select(&mut state, first.clone());
        state.tab = tab;

        delete_confirmed(&mut state, first);

        assert_eq!(state.app.current_session(), None);
        assert_eq!(state.tab, Tab::Projects);
    }
}

#[test]
fn deleting_an_orchestrator_whose_child_is_open_shows_the_projects_tab() {
    let mut fixture = orchestrated();
    select(&mut fixture.state, fixture.child.clone());
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
    select(&mut state, second.clone());

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
    select(&mut state, first.clone());
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
    state.app.submit_prompt(prompt).unwrap();
    (state, second)
}

/// Feeds `update` for `session` through the application, as its backend would.
fn report(state: &mut State, session: &SessionId, record: TranscriptRecord) {
    feed(state, session, record);
}

/// The node of `session` in the tree of `state`, at any depth.
fn node(state: &State, session: &SessionId) -> SessionNode {
    state.app.tree().find_node(session).unwrap().clone()
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
        TranscriptRecord::Title("Worktree support".into()),
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
        TranscriptRecord::Usage {
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
        TranscriptRecord::Usage {
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
fn a_click_on_the_title_opens_the_session() {
    let dir = project_dir();
    // The newer session is the top row, whose title is found first.
    let (state, _backend, [_, second]) = state_with_project(&dir);

    let mut ui = simulator(view(&state));
    ui.click("untitled").unwrap();
    let messages: Vec<_> = ui
        .into_messages()
        .filter(|message| {
            matches!(
                message,
                crate::app::Message::SelectSession(_)
                    | crate::app::Message::Projects(ProjectsMessage::StartRename(_))
            )
        })
        .collect();
    assert_eq!(messages, [crate::app::Message::SelectSession(second)]);
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
fn picking_a_row_closes_the_title_field() {
    let dir = project_dir();
    let (mut state, _backend, [first, _]) = state_with_project(&dir);
    apply(
        &mut state,
        [
            ProjectsMessage::StartRename(first.clone()),
            ProjectsMessage::RenameInput("Dropped".into()),
        ],
    );
    select(&mut state, first.clone());
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
        TranscriptRecord::Usage {
            used: 68_000,
            size: 1_000_000,
        },
    );

    let mut ui = simulator(view(&state));
    let row = bounds_of(&mut ui, format!("session-{first}"));
    assert!(shows_within(&mut ui, "idle", row));
    assert!(shows_within(&mut ui, "68k / 1M", row));
}
