#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;

use futures::channel::mpsc;
use iced::Point;
use iced::widget::Id;
use iced_test::simulator;
use std::path::PathBuf;

use iced::event;
use iced::keyboard::key;
use partitur_core::testing::{
    Call, GitCall, GitOp, MemoryGit, MemoryStore, RecordingBackend, repo_info, single_claude,
};
use partitur_core::{
    App, BackendEvent, BackendId, BackendInfo, BackendSessionId, BranchMode, ConfigCategory,
    ConfigChoice, ConfigId, ConfigOption, ConfigValueId, GlobalSettings, ProjectId, RepositoryId,
    RequestId, Scope, SessionFailure, SessionId,
};
use tempfile::TempDir;

use super::*;
use crate::app::{BareKey, Message, State, Tab, boot, update as app_update, view as app_view};
use crate::projects_view::ProjectsMessage;
use crate::test_support::{orchestrated, settle_git, shown, two_backends};

/// A state over a recording backend, an in-memory store and an in-memory git with one
/// project open. Each of its repositories (only `api` by default) has `main` checked out,
/// a branch `dev` besides, and `origin/main` on the remote.
struct Fixture {
    state: State,
    backend: RecordingBackend,
    store: MemoryStore,
    git: MemoryGit,
    project: ProjectId,
    name: String,
    dir: TempDir,
    _drained: crate::test_support::Drained,
}

fn fixture_with(backends: (Vec<BackendInfo>, BackendId)) -> Fixture {
    fixture_in(backends, &["api"])
}

fn fixture_in(backends: (Vec<BackendInfo>, BackendId), repositories: &[&str]) -> Fixture {
    let dir = TempDir::new().unwrap();
    let backend = RecordingBackend::new();
    let store = MemoryStore::new();
    let git = MemoryGit::new();
    let root = partitur_core::canonical(dir.path()).unwrap();
    for name in repositories {
        let path = root.join(name);
        fs::create_dir_all(path.join(".git")).unwrap();
        git.add_repository(
            path.clone(),
            repo_info(&path, "main", &["dev", "main"], &["origin/main"]),
        );
    }
    let (backends, default) = backends;
    let app = App::load(
        Box::new(backend.clone()),
        Box::new(store.clone()),
        Box::new(store.clone()),
        Box::new(git.clone()),
        backends,
        default,
        GlobalSettings::default(),
        Box::new(partitur_core::testing::FixedClock::default()),
    )
    .unwrap();
    let mut state = boot(app, mpsc::unbounded().1, mpsc::unbounded().1).0;
    let project = state.app.open_project(dir.path()).unwrap();
    let name = state.app.tree().projects[0].name.clone();
    Fixture {
        state,
        backend,
        _drained: crate::test_support::Drained(store.clone()),
        store,
        git,
        project,
        name,
        dir,
    }
}

fn fixture() -> Fixture {
    fixture_with(single_claude())
}

/// A fixture whose project folder is itself a repository, so it has no other.
fn rooted_fixture() -> Fixture {
    let mut fixture = fixture_in(single_claude(), &[]);
    let root = partitur_core::canonical(fixture.dir.path()).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fixture
        .git
        .add_repository(root.clone(), repo_info(&root, "main", &["main"], &[]));
    fixture.project = fixture.state.app.open_project(&root).unwrap();
    fixture
}

fn send(state: &mut State, message: Message) {
    let _task = app_update(state, message);
}

fn sheet(message: NewSessionMessage) -> Message {
    Message::NewSession(message)
}

fn open_project_sheet(fixture: &mut Fixture) {
    send(
        &mut fixture.state,
        Message::Projects(ProjectsMessage::NewSession(
            fixture.project.clone(),
            Scope::Project,
        )),
    );
}

/// The folder of the repository `api`, as the project records it.
fn api(fixture: &Fixture) -> PathBuf {
    partitur_core::canonical(fixture.dir.path())
        .unwrap()
        .join("api")
}

/// Opens the sheet for a session in the repository `api`; git has not answered yet.
fn open_api_sheet(fixture: &mut Fixture) {
    send(
        &mut fixture.state,
        Message::Projects(ProjectsMessage::NewSession(
            fixture.project.clone(),
            Scope::Repository(RepositoryId::try_from("api").unwrap()),
        )),
    );
}

/// Opens the sheet for `api`, lets its agent and git answer, and picks Worktree mode
/// with the branch `feat/x`.
fn worktree_sheet() -> Fixture {
    let mut fixture = fixture();
    open_api_sheet(&mut fixture);
    answer(&mut fixture.state, &fixture.backend, Vec::new());
    let _task = settle_git(&mut fixture.state, &fixture.git);
    send(
        &mut fixture.state,
        sheet(NewSessionMessage::Mode(BranchMode::Worktree)),
    );
    send(
        &mut fixture.state,
        sheet(NewSessionMessage::Branch("feat/x".into())),
    );
    fixture
}

/// The messages a click on the control `id` of the window sends.
fn click(state: &State, id: &'static str) -> Vec<Message> {
    let mut ui = simulator(app_view(state));
    ui.click(Id::new(id)).unwrap();
    ui.into_messages().collect()
}

/// The messages produced by opening the Repo field and clicking its second entry, one
/// picker height below the first. A frame produces none; an enabled pick list sends
/// `Scope`.
fn pick_second_repo(state: &State) -> Vec<Message> {
    let mut ui = simulator(app_view(state));
    let picker = ui.find(Id::new("new-session-repo")).unwrap().bounds();
    ui.point_at(picker.center());
    let _ = ui.simulate(iced_test::simulator::click());
    let second = Point::new(picker.x + 12.0, picker.y + picker.height * 2.5);
    ui.point_at(second);
    let moved = iced::Event::Mouse(iced::mouse::Event::CursorMoved { position: second });
    let _ = ui.simulate(std::iter::once(moved).chain(iced_test::simulator::click()));
    ui.into_messages().collect()
}

/// How many texts of the window read `label`.
fn count(state: &State, label: &str) -> usize {
    let mut ui = simulator(app_view(state));
    shown(&mut ui, label)
}

/// The request and the id of the latest warm session the backend was asked to start.
fn warm_up(backend: &RecordingBackend) -> (RequestId, SessionId) {
    backend
        .calls()
        .iter()
        .filter_map(Call::as_new_session)
        .map(|(request, session, _)| (request, session.clone()))
        .next_back()
        .unwrap()
}

fn warm_session(backend: &RecordingBackend) -> SessionId {
    warm_up(backend).1
}

fn new_session_calls(backend: &RecordingBackend) -> usize {
    backend
        .calls()
        .iter()
        .filter(|call| call.as_new_session().is_some())
        .count()
}

/// An option of `category` with the values `ids`, the first one current.
fn option(category: ConfigCategory, ids: &[&str]) -> ConfigOption {
    ConfigOption {
        id: ConfigId::new(format!("{category:?}").to_lowercase()),
        name: format!("{category:?}"),
        category,
        current: ConfigValueId::new(ids[0]),
        choices: ids
            .iter()
            .map(|id| ConfigChoice {
                id: ConfigValueId::new(*id),
                name: (*id).to_owned(),
            })
            .collect(),
    }
}

/// Answers the latest warm-up with `config`.
fn answer(state: &mut State, backend: &RecordingBackend, config: Vec<ConfigOption>) {
    let (request, session) = warm_up(backend);
    let _task = app_update(
        state,
        Message::Backend(BackendEvent::SessionCreated {
            request,
            session,
            backend_session: BackendSessionId::new("b-warm"),
            config,
        }),
    );
}

fn sessions_in_tree(state: &State) -> usize {
    state.app.tree().projects[0].sessions.len()
}

#[test]
fn clicking_new_session_opens_the_sheet_and_creates_nothing() {
    let mut fixture = fixture();
    let messages: Vec<Message> = {
        let mut ui = simulator(app_view(&fixture.state));
        ui.click(Id::from(format!("new-session-{}", fixture.project)))
            .unwrap();
        ui.into_messages().collect()
    };
    for message in messages {
        send(&mut fixture.state, message);
    }

    let mut ui = simulator(app_view(&fixture.state));
    ui.find(format!("New session in {}", fixture.name).as_str())
        .unwrap();
    ui.find("1 repo").unwrap();
    ui.find("loading…").unwrap();
    assert_eq!(sessions_in_tree(&fixture.state), 0);
    assert!(matches!(
        fixture.backend.calls().last(),
        Some(Call::NewSession { .. })
    ));
    assert_eq!(fixture.state.tab, Tab::Projects);
}

#[test]
fn n_opens_the_sheet_for_the_cursors_project() {
    let mut fixture = fixture();

    send(&mut fixture.state, Message::Key(BareKey::N));

    assert!(fixture.state.app.draft().is_some());
    assert_eq!(sessions_in_tree(&fixture.state), 0);
}

#[test]
fn the_title_names_only_the_project() {
    let mut fixture = fixture();

    open_api_sheet(&mut fixture);

    let mut ui = simulator(app_view(&fixture.state));
    ui.find(format!("New session in {}", fixture.name).as_str())
        .unwrap();
    assert!(
        ui.find(format!("New session in {} · api", fixture.name).as_str())
            .is_err()
    );
}

#[test]
fn the_repo_field_comes_first_and_lists_orchestrator_then_repos() {
    let mut fixture = fixture_in(single_claude(), &["api", "web"]);
    open_project_sheet(&mut fixture);

    let mut ui = simulator(app_view(&fixture.state));
    let repo = ui.find(Id::new("new-session-repo")).unwrap().bounds();
    let model = ui.find(Id::new("new-session-model")).unwrap().bounds();
    assert!(repo.y < model.y);
    let labels: Vec<String> = fixture
        .state
        .app
        .draft()
        .unwrap()
        .scopes
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(labels, ["orchestrator · all 2 repos", "api", "web"]);
}

#[test]
fn picking_a_repo_makes_a_repository_session() {
    let mut fixture = fixture_in(single_claude(), &["api", "web"]);
    open_project_sheet(&mut fixture);
    send(
        &mut fixture.state,
        sheet(NewSessionMessage::Scope(api_choice())),
    );
    answer(&mut fixture.state, &fixture.backend, Vec::new());
    send(&mut fixture.state, sheet(NewSessionMessage::Create));

    let calls = fixture.backend.calls();
    let (_, session, cwd) = calls.last().and_then(Call::as_new_session).unwrap();
    assert_eq!(cwd, api(&fixture));
    assert_eq!(fixture.state.app.current_session().as_ref(), Some(session));
    assert_eq!(
        fixture.state.app.tree().projects[0].sessions[0].scope_label,
        "api"
    );
}

/// The picker's entry for the repository `api`, which has no instruction files.
fn api_choice() -> ScopeChoice {
    ScopeChoice {
        scope: Scope::Repository(RepositoryId::try_from("api").unwrap()),
        orchestrator: false,
        label: "api".to_owned(),
    }
}

#[test]
fn a_pick_in_the_repo_field_sends_the_scope() {
    let mut fixture = fixture_in(single_claude(), &["api", "web"]);
    open_project_sheet(&mut fixture);
    let mut ui = simulator(app_view(&fixture.state));

    // A pick list keeps its text from selectors, so the menu is opened and its second
    // entry, one picker height below the first, is picked.
    let picker = ui.find(Id::new("new-session-repo")).unwrap().bounds();
    ui.point_at(picker.center());
    let _ = ui.simulate(iced_test::simulator::click());
    let second = Point::new(picker.x + 12.0, picker.y + picker.height * 2.5);
    ui.point_at(second);
    let moved = iced::Event::Mouse(iced::mouse::Event::CursorMoved { position: second });
    let _ = ui.simulate(std::iter::once(moved).chain(iced_test::simulator::click()));

    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        [sheet(NewSessionMessage::Scope(api_choice()))]
    );
}

#[test]
fn the_repo_entries_name_their_instruction_files() {
    let mut fixture = fixture_in(single_claude(), &["api", "web"]);
    fs::write(fixture.dir.path().join("api").join("CLAUDE.md"), "x").unwrap();
    // The project reads its instruction files when it is opened, so it is opened again
    // to see the one just written.
    let _ = fixture.state.app.close_project(fixture.project.clone());
    let project = fixture.state.app.open_project(fixture.dir.path()).unwrap();

    send(
        &mut fixture.state,
        Message::Projects(ProjectsMessage::NewSession(project, Scope::Project)),
    );

    let labels: Vec<String> = fixture
        .state
        .app
        .draft()
        .unwrap()
        .scopes
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        labels,
        ["orchestrator · all 2 repos", "api · CLAUDE.md", "web"]
    );
}

#[test]
fn a_root_repo_sheet_can_pick_the_orchestrator() {
    let mut fixture = rooted_fixture();
    open_project_sheet(&mut fixture);
    assert_eq!(fixture.state.app.draft().unwrap().scopes.len(), 2);

    let mut ui = simulator(app_view(&fixture.state));
    ui.find(Id::new("new-session-repo")).unwrap();
    ui.find(fixture.name.as_str()).unwrap();
    let picked = pick_second_repo(&fixture.state);
    let picks_orchestrator = matches!(
        picked.as_slice(),
        [Message::NewSession(NewSessionMessage::Scope(choice))]
            if choice.scope == Scope::Project && choice.orchestrator
    );
    assert!(picks_orchestrator, "{picked:?}");
}

#[test]
fn a_folder_around_one_repo_can_pick_it() {
    let mut fixture = fixture();
    open_project_sheet(&mut fixture);

    let picked = pick_second_repo(&fixture.state);
    let wanted = Scope::Repository(RepositoryId::try_from("api").unwrap());
    let picks_api = matches!(
        picked.as_slice(),
        [Message::NewSession(NewSessionMessage::Scope(choice))]
            if choice.scope == wanted && !choice.orchestrator
    );
    assert!(picks_api, "{picked:?}");
    for message in picked {
        send(&mut fixture.state, message);
    }
    answer(&mut fixture.state, &fixture.backend, Vec::new());
    send(&mut fixture.state, sheet(NewSessionMessage::Create));

    let calls = fixture.backend.calls();
    let (_, _, cwd) = calls.last().and_then(Call::as_new_session).unwrap();
    assert_eq!(cwd, api(&fixture));
    assert_eq!(
        fixture.state.app.tree().projects[0].sessions[0].scope_label,
        "api"
    );
}

#[test]
fn the_repo_field_is_frozen_while_creating() {
    let mut fixture = fixture_in(single_claude(), &["api", "web"]);
    open_project_sheet(&mut fixture);
    send(
        &mut fixture.state,
        sheet(NewSessionMessage::Scope(api_choice())),
    );
    answer(&mut fixture.state, &fixture.backend, Vec::new());
    let _task = settle_git(&mut fixture.state, &fixture.git);
    for message in [
        NewSessionMessage::Mode(BranchMode::Worktree),
        NewSessionMessage::Branch("feat/x".into()),
        NewSessionMessage::Create,
    ] {
        send(&mut fixture.state, sheet(message));
    }
    assert!(fixture.state.app.draft().unwrap().creating);

    assert_eq!(pick_second_repo(&fixture.state), []);

    send(
        &mut fixture.state,
        sheet(NewSessionMessage::Scope(api_choice())),
    );
    assert!(fixture.state.sheet_error.is_some());
}

#[test]
fn the_subline_says_children_inherit() {
    let mut fixture = fixture_in(single_claude(), &["api", "web"]);
    open_project_sheet(&mut fixture);
    let mut ui = simulator(app_view(&fixture.state));
    ui.find("2 repos · children of an orchestrator inherit these settings")
        .unwrap();

    let mut single = fixture_in(single_claude(), &["api"]);
    open_project_sheet(&mut single);
    let mut ui = simulator(app_view(&single.state));
    ui.find("1 repo").unwrap();
}

#[test]
fn the_subline_promises_no_orchestrator_where_there_can_be_none() {
    let mut empty = fixture_in(single_claude(), &[]);
    let behind = count(&empty.state, "0 repos");
    open_project_sheet(&mut empty);
    assert_eq!(count(&empty.state, "0 repos"), behind + 1);
    assert_eq!(
        count(
            &empty.state,
            "0 repos · children of an orchestrator inherit these settings"
        ),
        0
    );

    let mut rooted = rooted_fixture();
    open_project_sheet(&mut rooted);
    let mut ui = simulator(app_view(&rooted.state));
    ui.find("1 repo").unwrap();
}

#[test]
fn a_single_repo_subline_stays_without_the_orchestrator_note() {
    let note = "1 repo · children of an orchestrator inherit these settings";
    for mut fixture in [fixture_in(single_claude(), &["api"]), rooted_fixture()] {
        open_project_sheet(&mut fixture);
        assert_eq!(count(&fixture.state, note), 0);
    }
}

#[test]
fn the_picker_shows_the_orchestrator_as_picked() {
    let mut fixture = rooted_fixture();
    open_project_sheet(&mut fixture);
    let label = "orchestrator · for this repository";
    assert_eq!(count(&fixture.state, label), 0);
    let choice = fixture.state.app.draft().unwrap().scopes[1].clone();
    assert!(choice.orchestrator);

    send(&mut fixture.state, sheet(NewSessionMessage::Scope(choice)));

    let draft = fixture.state.app.draft().unwrap();
    assert!(draft.orchestrator);
    // A pick list draws its selection itself, so it is read from what the picker gets.
    assert_eq!(
        picked_scope(&draft).map(|picked| picked.label),
        Some(label.to_owned())
    );
}

#[test]
fn an_orchestrator_sheet_says_children_inherit() {
    let mut fixture = orchestrated();
    let project = fixture.state.app.tree().projects[0].id.clone();
    fixture.state.tab = Tab::Projects;

    send(
        &mut fixture.state,
        Message::Projects(ProjectsMessage::NewSession(project, Scope::Project)),
    );

    let mut ui = simulator(app_view(&fixture.state));
    ui.find("2 repos · children of an orchestrator inherit these settings")
        .unwrap();
}

#[test]
fn the_agents_answer_fills_the_pickers() {
    let mut fixture = fixture();
    open_project_sheet(&mut fixture);

    answer(
        &mut fixture.state,
        &fixture.backend,
        vec![option(ConfigCategory::Model, &["Fast", "opus"])],
    );

    let draft = fixture.state.app.draft().unwrap();
    assert!(matches!(
        &draft.model,
        DraftChoice::Offered(choice) if choice.current_name == "Fast"
    ));
    let mut ui = simulator(app_view(&fixture.state));
    assert!(ui.find("loading…").is_err());
    ui.find("not offered by this backend").unwrap();

    // A pick list keeps its text from selectors, so the menu is opened and its second
    // entry, one picker height below the first, is picked.
    let picker = ui.find(Id::new("new-session-model")).unwrap().bounds();
    ui.point_at(picker.center());
    let _ = ui.simulate(iced_test::simulator::click());
    let second = Point::new(picker.x + 12.0, picker.y + picker.height * 2.5);
    ui.point_at(second);
    let moved = iced::Event::Mouse(iced::mouse::Event::CursorMoved { position: second });
    let _ = ui.simulate(std::iter::once(moved).chain(iced_test::simulator::click()));
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(
        messages,
        [sheet(NewSessionMessage::Model(ConfigValueId::new("opus")))]
    );
}

#[test]
fn the_sheet_no_longer_shows_placeholders() {
    let mut fixture = fixture();
    open_project_sheet(&mut fixture);

    let mut ui = simulator(app_view(&fixture.state));
    for label in [
        "Model",
        "Effort",
        "Backend",
        "Permissions",
        "Auto-allow tool calls",
        "Every tool call runs without asking.",
        "Branch & worktree",
        "mode",
        "Checked-out",
        "the project folder is not a repository",
        "Create session",
        "Enter",
        "Cancel",
        "Esc",
        "title comes from your first prompt",
        "default",
    ] {
        assert!(shown(&mut ui, label) >= 1, "{label} is not shown");
    }
    for gone in [
        "defaults · editable in a later version",
        "wt/<title>",
        "per repo, created on first prompt",
        // A project folder that is not a repository offers no other mode.
        "Worktree",
        "New branch",
        "works in the repository as it is now",
    ] {
        assert_eq!(shown(&mut ui, gone), 0, "{gone} is shown");
    }
}

#[test]
fn the_box_shows_the_checked_out_branch_by_default() {
    let mut fixture = fixture();
    open_api_sheet(&mut fixture);
    let _task = settle_git(&mut fixture.state, &fixture.git);

    let mut ui = simulator(app_view(&fixture.state));
    assert_eq!(shown(&mut ui, "works in the repository as it is now"), 1);
    assert_eq!(shown(&mut ui, "main"), 1);
    for mode in ["Worktree", "New branch", "Checked-out"] {
        assert_eq!(shown(&mut ui, mode), 1, "{mode}");
    }
    for row in ["branch name", "base branch", "worktree at"] {
        assert_eq!(shown(&mut ui, row), 0, "{row}");
    }
}

#[test]
fn worktree_mode_shows_name_base_fetch_and_the_resolved_path() {
    let mut fixture = fixture();
    open_api_sheet(&mut fixture);
    let _task = settle_git(&mut fixture.state, &fixture.git);

    let messages = click(&fixture.state, "new-session-mode-worktree");
    assert_eq!(
        messages,
        [sheet(NewSessionMessage::Mode(BranchMode::Worktree))]
    );
    for message in messages {
        send(&mut fixture.state, message);
    }
    send(
        &mut fixture.state,
        sheet(NewSessionMessage::Branch("feat/x".into())),
    );

    let worktree = api(&fixture)
        .parent()
        .unwrap()
        .join("api-wt")
        .join("feat")
        .join("x");
    let path = home_path::abbreviated(&worktree, std::env::home_dir().as_deref());
    let mut ui = simulator(app_view(&fixture.state));
    for label in [
        "branch name",
        "base branch",
        "Fetch origin and start from origin/main",
        "worktree at",
        path.as_str(),
    ] {
        assert_eq!(shown(&mut ui, label), 1, "{label}");
    }
    assert_eq!(shown(&mut ui, "works in the repository as it is now"), 0);
    assert_eq!(fixture.state.sheet_error, None);
}

#[test]
fn a_base_without_a_remote_counterpart_cannot_be_fetched() {
    let mut fixture = worktree_sheet();

    send(
        &mut fixture.state,
        sheet(NewSessionMessage::Base("dev".into())),
    );

    assert_eq!(count(&fixture.state, "dev has no remote counterpart"), 1);
    assert!(click(&fixture.state, "new-session-fetch").is_empty());
    assert!(!fixture.state.app.draft().unwrap().branch.fetch);
}

#[test]
fn create_is_disabled_and_says_why_until_the_name_is_valid() {
    let mut fixture = worktree_sheet();
    send(
        &mut fixture.state,
        sheet(NewSessionMessage::Branch(String::new())),
    );
    let calls = fixture.git.calls().len();

    assert_eq!(count(&fixture.state, "enter a branch name"), 1);
    assert!(click(&fixture.state, "new-session-create").is_empty());
    send(&mut fixture.state, Message::Key(BareKey::Enter));
    assert!(!fixture.state.app.draft().unwrap().creating);
    assert_eq!(fixture.git.calls().len(), calls);

    send(
        &mut fixture.state,
        sheet(NewSessionMessage::Branch("feat/x".into())),
    );
    assert_eq!(count(&fixture.state, "enter a branch name"), 0);
    assert_eq!(
        click(&fixture.state, "new-session-create"),
        [sheet(NewSessionMessage::Create)]
    );
}

#[test]
fn typing_n_in_the_name_field_does_not_reach_the_window() {
    let mut fixture = worktree_sheet();
    send(
        &mut fixture.state,
        sheet(NewSessionMessage::Branch(String::new())),
    );

    let mut ui = simulator(app_view(&fixture.state));
    ui.click(Id::new("new-session-branch")).unwrap();
    // A key the field takes is no shortcut of the window, where N opens a sheet.
    assert_eq!(ui.typewrite("n"), event::Status::Captured);
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(messages, [sheet(NewSessionMessage::Branch("n".into()))]);
    for message in messages {
        send(&mut fixture.state, message);
    }

    assert_eq!(fixture.state.app.draft().unwrap().branch.branch, "n");
    assert_eq!(fixture.state.sheet_error, None);
}

#[test]
fn enter_in_the_name_field_creates_the_session() {
    let mut fixture = worktree_sheet();

    let mut ui = simulator(app_view(&fixture.state));
    ui.click(Id::new("new-session-branch")).unwrap();
    // The field lets Enter through, so the window's shortcut creates the session.
    assert_eq!(ui.tap_key(key::Named::Enter), event::Status::Ignored);
    assert_eq!(ui.into_messages().count(), 0);
    send(&mut fixture.state, Message::Key(BareKey::Enter));

    assert!(fixture.state.app.draft().unwrap().creating);
    let _task = settle_git(&mut fixture.state, &fixture.git);
    assert_eq!(fixture.state.app.draft(), None);
    assert_eq!(sessions_in_tree(&fixture.state), 1);
}

#[test]
fn new_branch_mode_hints_that_the_repository_switches() {
    let mut fixture = worktree_sheet();

    send(
        &mut fixture.state,
        sheet(NewSessionMessage::Mode(BranchMode::NewBranch)),
    );

    let mut ui = simulator(app_view(&fixture.state));
    assert_eq!(shown(&mut ui, "switches api itself to the new branch"), 1);
    assert_eq!(shown(&mut ui, "branch name"), 1);
    assert_eq!(shown(&mut ui, "worktree at"), 0);
}

#[test]
fn the_branch_rows_say_loading_until_git_answers() {
    let mut fixture = fixture();
    open_api_sheet(&mut fixture);
    answer(&mut fixture.state, &fixture.backend, Vec::new());

    // Checked-out mode waits for the branch checked out now.
    {
        let mut ui = simulator(app_view(&fixture.state));
        assert_eq!(shown(&mut ui, "loading…"), 1);
        assert_eq!(shown(&mut ui, "works in the repository as it is now"), 1);
    }

    // The mode and the name can be changed meanwhile; base and fetch wait.
    let messages = click(&fixture.state, "new-session-mode-worktree");
    for message in messages {
        send(&mut fixture.state, message);
    }
    send(
        &mut fixture.state,
        sheet(NewSessionMessage::Branch("feat/x".into())),
    );
    {
        let mut ui = simulator(app_view(&fixture.state));
        assert_eq!(shown(&mut ui, "loading…"), 2);
        assert_eq!(shown(&mut ui, "reading the branches of api…"), 1);
    }
    assert!(click(&fixture.state, "new-session-fetch").is_empty());
    assert_eq!(fixture.state.app.draft().unwrap().branch.branch, "feat/x");

    let _task = settle_git(&mut fixture.state, &fixture.git);
    let mut ui = simulator(app_view(&fixture.state));
    assert_eq!(shown(&mut ui, "loading…"), 0);
    assert_eq!(shown(&mut ui, "Fetch origin and start from origin/main"), 1);
    let branch = fixture.state.app.draft().unwrap().branch;
    assert_eq!(branch.base.as_deref(), Some("main"));
}

#[test]
fn create_shows_creating_and_waits_for_git() {
    let mut fixture = worktree_sheet();

    let messages = click(&fixture.state, "new-session-create");
    assert_eq!(messages, [sheet(NewSessionMessage::Create)]);
    for message in messages {
        send(&mut fixture.state, message);
    }

    assert_eq!(count(&fixture.state, "creating…"), 1);
    assert_eq!(count(&fixture.state, "Create session"), 0);
    assert!(fixture.state.app.draft().unwrap().creating);
    let calls = fixture.git.calls().len();
    assert!(click(&fixture.state, "new-session-create").is_empty());
    send(&mut fixture.state, Message::Key(BareKey::Enter));
    assert_eq!(fixture.git.calls().len(), calls);
    assert_eq!(sessions_in_tree(&fixture.state), 0);

    let task = settle_git(&mut fixture.state, &fixture.git);

    assert!(task.units() > 0);
    assert_eq!(fixture.state.app.draft(), None);
    assert_eq!(fixture.state.tab, Tab::Session);
    let session = fixture.state.app.current_session().unwrap();
    assert_eq!(fixture.state.projects.cursor, Some(session));
    assert_eq!(sessions_in_tree(&fixture.state), 1);
    assert_eq!(fixture.state.error, None);
    assert_eq!(fixture.state.sheet_error, None);
}

#[test]
fn the_sheet_stays_disabled_while_creating() {
    let mut fixture = fixture_with(two_backends());
    open_api_sheet(&mut fixture);
    answer(
        &mut fixture.state,
        &fixture.backend,
        vec![option(ConfigCategory::Model, &["sonnet", "opus"])],
    );
    let _task = settle_git(&mut fixture.state, &fixture.git);
    for message in [
        NewSessionMessage::Mode(BranchMode::Worktree),
        NewSessionMessage::Branch("feat/x".into()),
        NewSessionMessage::Create,
    ] {
        send(&mut fixture.state, sheet(message));
    }
    let draft = fixture.state.app.draft();

    let mut ui = simulator(app_view(&fixture.state));
    for id in [
        "new-session-mode-checked-out",
        "new-session-mode-new-branch",
        "new-session-fetch",
        "new-session-auto-allow",
        "new-session-base",
        "new-session-model",
        "new-session-backend",
        "new-session-branch",
    ] {
        ui.click(Id::new(id)).unwrap();
    }
    let _ = ui.typewrite("y");
    assert_eq!(ui.into_messages().count(), 0);
    // The pickers are frames that show the values picked.
    assert_eq!(count(&fixture.state, "sonnet"), 1);
    assert_eq!(count(&fixture.state, "claude-main"), 1);

    // A change that gets through anyway is refused by the core, in the sheet.
    send(&mut fixture.state, sheet(NewSessionMessage::Fetch(false)));
    assert!(fixture.state.sheet_error.is_some());
    assert_eq!(fixture.state.app.draft(), draft);
}

#[test]
fn a_git_failure_on_create_shows_in_the_sheet() {
    let mut fixture = worktree_sheet();
    fixture.git.fail_next(GitOp::AddWorktree, &api(&fixture));

    send(&mut fixture.state, sheet(NewSessionMessage::Create));
    let _task = settle_git(&mut fixture.state, &fixture.git);

    let draft = fixture.state.app.draft().unwrap();
    let failure = draft.failure.clone().unwrap();
    assert!(!draft.creating);
    assert_eq!(count(&fixture.state, &failure), 1);
    assert_eq!(count(&fixture.state, "Create session"), 1);
    assert_eq!(
        click(&fixture.state, "new-session-create"),
        [sheet(NewSessionMessage::Create)]
    );
    assert_eq!(sessions_in_tree(&fixture.state), 0);
    assert_eq!(fixture.state.tab, Tab::Projects);
    assert_eq!(fixture.state.error, None);
}

#[test]
fn cancel_while_creating_closes_the_sheet_at_once() {
    let mut fixture = worktree_sheet();
    send(&mut fixture.state, sheet(NewSessionMessage::Create));
    // Git reads the repository again, and the worktree is queued.
    for event in fixture.git.answer() {
        send(&mut fixture.state, Message::Git(event));
    }
    assert!(fixture.state.app.draft().unwrap().creating);

    let messages = click(&fixture.state, "new-session-cancel");
    assert_eq!(messages, [sheet(NewSessionMessage::Cancel)]);
    for message in messages {
        send(&mut fixture.state, message);
    }
    assert_eq!(fixture.state.app.draft(), None);

    let _task = settle_git(&mut fixture.state, &fixture.git);
    assert_eq!(sessions_in_tree(&fixture.state), 0);
    assert_eq!(fixture.state.tab, Tab::Projects);
    assert_eq!(fixture.state.error, None);
    let calls = fixture.git.calls();
    assert!(
        matches!(
            calls.as_slice(),
            [
                ..,
                GitCall::AddWorktree { .. },
                GitCall::RemoveWorktree { .. },
                GitCall::DeleteBranch { .. }
            ]
        ),
        "{calls:?}"
    );
}

#[test]
fn escape_while_git_reads_again_closes_the_sheet_and_leaves_nothing() {
    let mut fixture = worktree_sheet();
    send(&mut fixture.state, sheet(NewSessionMessage::Create));

    send(&mut fixture.state, Message::Key(BareKey::Escape));
    assert_eq!(fixture.state.app.draft(), None);
    let _task = settle_git(&mut fixture.state, &fixture.git);

    assert_eq!(sessions_in_tree(&fixture.state), 0);
    assert_eq!(fixture.state.error, None);
    assert!(
        fixture
            .git
            .calls()
            .iter()
            .all(|call| matches!(call, GitCall::Inspect(_))),
    );
}

#[test]
fn the_new_controls_are_found_by_their_ids() {
    let mut fixture = worktree_sheet();
    {
        let mut ui = simulator(app_view(&fixture.state));
        for id in [
            "new-session-mode-worktree",
            "new-session-mode-new-branch",
            "new-session-mode-checked-out",
            "new-session-branch",
            "new-session-base",
            "new-session-fetch",
        ] {
            ui.find(Id::new(id)).unwrap();
        }
    }
    for (id, expected) in [
        (
            "new-session-mode-new-branch",
            NewSessionMessage::Mode(BranchMode::NewBranch),
        ),
        (
            "new-session-mode-checked-out",
            NewSessionMessage::Mode(BranchMode::CheckedOut),
        ),
        ("new-session-fetch", NewSessionMessage::Fetch(false)),
    ] {
        assert_eq!(click(&fixture.state, id), [sheet(expected)]);
    }

    // The base picker opens its menu above it, since the picker sits low in the window,
    // and the menu's first entry, one and a half picker heights up, is picked.
    let mut ui = simulator(app_view(&fixture.state));
    let picker = ui.find(Id::new("new-session-base")).unwrap().bounds();
    ui.point_at(picker.center());
    let _ = ui.simulate(iced_test::simulator::click());
    let first = Point::new(picker.x + 12.0, picker.y - picker.height * 1.5);
    ui.point_at(first);
    let moved = iced::Event::Mouse(iced::mouse::Event::CursorMoved { position: first });
    let _ = ui.simulate(std::iter::once(moved).chain(iced_test::simulator::click()));
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(messages, [sheet(NewSessionMessage::Base("dev".into()))]);
    for message in messages {
        send(&mut fixture.state, message);
    }
    assert_eq!(
        fixture.state.app.draft().unwrap().branch.base.as_deref(),
        Some("dev")
    );
}

#[test]
fn an_orchestrator_sheet_picks_the_childrens_branch_settings() {
    let mut fixture = fixture_in(single_claude(), &["api", "web"]);
    open_project_sheet(&mut fixture);
    let _task = settle_git(&mut fixture.state, &fixture.git);
    assert_eq!(
        count(
            &fixture.state,
            "children work in their repositories as they are now"
        ),
        1
    );

    send(
        &mut fixture.state,
        sheet(NewSessionMessage::Mode(BranchMode::Worktree)),
    );

    let mut ui = simulator(app_view(&fixture.state));
    for label in [
        "branch name",
        "Fetch first and start from each repository's remote base",
    ] {
        assert_eq!(shown(&mut ui, label), 1, "{label}");
    }
    assert_eq!(shown(&mut ui, "worktree at"), 0);
    let messages: Vec<Message> = {
        ui.click(Id::new("new-session-fetch")).unwrap();
        ui.into_messages().collect()
    };
    assert_eq!(messages, [sheet(NewSessionMessage::Fetch(false))]);
}

#[test]
fn a_branch_change_the_core_refuses_shows_in_the_sheet() {
    let mut fixture = worktree_sheet();

    send(
        &mut fixture.state,
        sheet(NewSessionMessage::Base("gone".into())),
    );

    let error = fixture.state.sheet_error.clone().unwrap();
    assert_eq!(count(&fixture.state, &error), 1);
    assert_eq!(fixture.state.error, None);
}

#[test]
fn picking_values_reaches_the_core() {
    let mut fixture = fixture_with(two_backends());
    open_project_sheet(&mut fixture);
    answer(
        &mut fixture.state,
        &fixture.backend,
        vec![
            option(ConfigCategory::Model, &["sonnet", "opus"]),
            option(ConfigCategory::Effort, &["low", "high"]),
        ],
    );

    send(
        &mut fixture.state,
        sheet(NewSessionMessage::Model(ConfigValueId::new("opus"))),
    );
    assert!(matches!(
        fixture.backend.calls().last(),
        Some(Call::SetConfig { value, .. }) if value == &ConfigValueId::new("opus")
    ));

    send(
        &mut fixture.state,
        sheet(NewSessionMessage::Effort(ConfigValueId::new("high"))),
    );
    let draft = fixture.state.app.draft().unwrap();
    assert!(matches!(
        draft.effort,
        DraftChoice::Offered(choice) if choice.current == ConfigValueId::new("high")
    ));

    send(
        &mut fixture.state,
        sheet(NewSessionMessage::AutoAllow(true)),
    );
    assert!(fixture.state.app.draft().unwrap().auto_allow);

    let warm = warm_session(&fixture.backend);
    let work = BackendId::try_from("claude-work").unwrap();
    send(
        &mut fixture.state,
        sheet(NewSessionMessage::Backend(work.clone())),
    );
    let calls = fixture.backend.calls();
    let n = calls.len();
    assert_eq!(calls[n - 2], Call::CloseSession { session: warm });
    assert!(matches!(
        &calls[n - 1],
        Call::NewSession { backend, .. } if backend == &work
    ));
    assert_eq!(fixture.state.app.draft().unwrap().backend, work);
    assert_eq!(fixture.state.error, None);
}

#[test]
fn a_failed_warm_up_shows_the_error_and_disables_create() {
    let mut fixture = fixture();
    open_project_sheet(&mut fixture);
    let (request, session) = warm_up(&fixture.backend);
    send(
        &mut fixture.state,
        Message::Backend(BackendEvent::RequestFailed {
            request,
            session,
            error: SessionFailure::RequestFailed("no login".into()),
        }),
    );

    {
        let mut ui = simulator(app_view(&fixture.state));
        ui.find("no login").unwrap();
        ui.click(Id::new("new-session-create")).unwrap();
        assert_eq!(ui.into_messages().count(), 0);
    }

    send(&mut fixture.state, Message::Key(BareKey::Enter));
    assert!(fixture.state.app.draft().is_some());
    assert_eq!(sessions_in_tree(&fixture.state), 0);
}

#[test]
fn enter_creates_selects_and_focuses_the_composer() {
    let mut fixture = fixture();
    open_project_sheet(&mut fixture);
    let warm = warm_session(&fixture.backend);
    answer(&mut fixture.state, &fixture.backend, Vec::new());

    let task = app_update(&mut fixture.state, Message::Key(BareKey::Enter));

    assert!(task.units() > 0);
    assert_eq!(fixture.state.tab, Tab::Session);
    assert_eq!(fixture.state.app.current_session(), Some(warm.clone()));
    assert_eq!(fixture.state.projects.cursor, Some(warm));
    assert_eq!(fixture.state.app.draft(), None);
    assert_eq!(fixture.state.error, None);
    assert_eq!(sessions_in_tree(&fixture.state), 1);
}

#[test]
fn escape_cancels_and_leaves_nothing() {
    let mut fixture = fixture();
    open_project_sheet(&mut fixture);
    let warm = warm_session(&fixture.backend);

    send(&mut fixture.state, Message::Key(BareKey::Escape));

    assert_eq!(
        fixture.backend.calls().last(),
        Some(&Call::CloseSession { session: warm })
    );
    assert_eq!(fixture.state.app.draft(), None);
    assert_eq!(sessions_in_tree(&fixture.state), 0);
    assert_eq!(fixture.store.session_saves(&fixture.project), 0);
    assert_eq!(fixture.state.tab, Tab::Projects);
}

#[test]
fn clicking_the_backdrop_cancels() {
    let mut fixture = fixture();
    open_project_sheet(&mut fixture);
    let mut ui = simulator(app_view(&fixture.state));

    ui.point_at(Point::new(20.0, 600.0));
    ui.simulate(iced_test::simulator::click());

    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(messages, [sheet(NewSessionMessage::Cancel)]);
}

#[test]
fn the_buttons_create_and_cancel() {
    let mut fixture = fixture();
    open_project_sheet(&mut fixture);

    for (button, expected) in [
        ("new-session-create", NewSessionMessage::Create),
        ("new-session-cancel", NewSessionMessage::Cancel),
    ] {
        let mut ui = simulator(app_view(&fixture.state));
        ui.click(Id::new(button)).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [sheet(expected)]);
    }
}

#[test]
fn the_pickers_and_the_checkbox_are_found_by_their_ids() {
    let mut fixture = fixture();
    open_project_sheet(&mut fixture);
    {
        let mut ui = simulator(app_view(&fixture.state));
        for id in [
            "new-session-model",
            "new-session-effort",
            "new-session-backend",
            "new-session-auto-allow",
        ] {
            ui.find(Id::new(id)).unwrap();
        }
    }

    let mut ui = simulator(app_view(&fixture.state));
    ui.click(Id::new("new-session-auto-allow")).unwrap();
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(messages, [sheet(NewSessionMessage::AutoAllow(true))]);
}

#[test]
fn keys_other_than_enter_and_escape_do_nothing_while_the_sheet_is_open() {
    let mut fixture = fixture();
    open_project_sheet(&mut fixture);
    let draft = fixture.state.app.draft();
    let starts = new_session_calls(&fixture.backend);

    for key in [
        BareKey::Tab,
        BareKey::ArrowDown,
        BareKey::ArrowUp,
        BareKey::N,
        BareKey::A,
        BareKey::D,
    ] {
        send(&mut fixture.state, Message::Key(key));
    }
    send(
        &mut fixture.state,
        Message::Jump(crate::jump::JumpMessage::Open),
    );
    send(&mut fixture.state, Message::SelectTab(Tab::Env));

    assert_eq!(fixture.state.tab, Tab::Projects);
    assert_eq!(fixture.state.jump, None);
    assert_eq!(fixture.state.app.draft(), draft);
    assert_eq!(new_session_calls(&fixture.backend), starts);
}

#[test]
fn a_create_that_waits_for_git_keeps_the_sheet_open() {
    let mut fixture = worktree_sheet();
    fixture.state.sheet_error = Some("an earlier error".into());

    send(&mut fixture.state, sheet(NewSessionMessage::Create));

    assert!(fixture.state.app.draft().unwrap().creating);
    assert_eq!(fixture.state.sheet_error, None);
    assert_eq!(fixture.state.error, None);
    assert_eq!(sessions_in_tree(&fixture.state), 0);
}

#[test]
fn a_git_answer_for_another_request_leaves_the_sheet_alone() {
    let mut fixture = worktree_sheet();
    let task = app_update(
        &mut fixture.state,
        Message::Git(partitur_core::GitEvent::Answered {
            request: partitur_core::GitRequestId(999),
            answers: Vec::new(),
        }),
    );

    assert_eq!(task.units(), 0);
    assert!(fixture.state.app.draft().is_some());
    assert_eq!(fixture.state.tab, Tab::Projects);
}

#[test]
fn a_create_whose_save_fails_shows_the_session_and_then_the_error() {
    let mut fixture = fixture();
    open_project_sheet(&mut fixture);

    fixture.store.fail_next_session_save();
    send(&mut fixture.state, Message::Key(BareKey::Enter));

    assert_eq!(fixture.state.app.draft(), None);
    assert_eq!(fixture.state.sheet_error, None);
    assert_eq!(sessions_in_tree(&fixture.state), 1);
    crate::test_support::settle_store(&mut fixture.state, &fixture.store);
    let error = fixture.state.error.clone().unwrap();
    assert!(error.contains("sessions.json"), "{error}");
    let mut ui = simulator(app_view(&fixture.state));
    assert_eq!(shown(&mut ui, &error), 1);
}

#[test]
fn an_error_from_elsewhere_stays_in_the_error_bar() {
    let mut fixture = fixture();
    let elsewhere = "another session failed";
    open_project_sheet(&mut fixture);

    send(&mut fixture.state, Message::Error(elsewhere.into()));

    {
        let mut ui = simulator(app_view(&fixture.state));
        assert_eq!(shown(&mut ui, elsewhere), 1);
    }
    send(&mut fixture.state, Message::Key(BareKey::Escape));
    assert_eq!(fixture.state.error.as_deref(), Some(elsewhere));

    open_project_sheet(&mut fixture);
    answer(&mut fixture.state, &fixture.backend, Vec::new());
    send(&mut fixture.state, Message::Key(BareKey::Enter));

    assert_eq!(sessions_in_tree(&fixture.state), 1);
    assert_eq!(fixture.state.error.as_deref(), Some(elsewhere));
}

#[test]
fn a_create_whose_choices_cannot_be_sent_shows_the_session_and_the_error() {
    let mut fixture = fixture();
    open_project_sheet(&mut fixture);
    let warm = warm_session(&fixture.backend);
    answer(
        &mut fixture.state,
        &fixture.backend,
        vec![option(ConfigCategory::Effort, &["low", "high"])],
    );
    send(
        &mut fixture.state,
        sheet(NewSessionMessage::Effort(ConfigValueId::new("high"))),
    );

    fixture.backend.fail_next_call();
    let task = app_update(&mut fixture.state, Message::Key(BareKey::Enter));

    assert!(task.units() > 0);
    assert_eq!(fixture.state.app.draft(), None);
    assert_eq!(fixture.state.tab, Tab::Session);
    assert_eq!(fixture.state.app.current_session(), Some(warm));
    assert!(fixture.state.error.is_some());
}

#[test]
fn a_failed_open_shows_the_error_and_no_sheet() {
    let mut fixture = fixture();

    send(
        &mut fixture.state,
        Message::Projects(ProjectsMessage::NewSession(
            ProjectId::new(),
            Scope::Project,
        )),
    );

    assert!(fixture.state.error.is_some());
    assert_eq!(fixture.state.app.draft(), None);
}

#[test]
fn a_closed_sheet_ignores_its_messages() {
    let mut fixture = fixture();

    for message in [
        NewSessionMessage::Create,
        NewSessionMessage::Cancel,
        NewSessionMessage::AutoAllow(true),
    ] {
        send(&mut fixture.state, sheet(message));
    }

    assert_eq!(sessions_in_tree(&fixture.state), 0);
    assert_eq!(fixture.state.error, None);
    assert!(fixture.backend.calls().is_empty());
}
