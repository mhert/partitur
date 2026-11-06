#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use tempfile::TempDir;

use super::*;
use crate::backend::{
    ConfigCategory, ConfigChoice, PermissionKind, PermissionOption, PromptContent, StopReason,
    ToolKind,
};
use crate::ids::{
    BackendSessionId, ConfigId, ConfigValueId, PermissionId, PermissionOptionId, ToolCallId,
};
use crate::instructions::{InstructionScope, inject, instruction_chain};
use crate::session::title_from_prompt;
use crate::testing::{Call, MemoryStore, RecordingBackend};
use crate::transcript::Entry;
use crate::view_model::{ChoiceView, InstructionFileView, SessionNode};

struct Fixture {
    app: App,
    backend: RecordingBackend,
    store: MemoryStore,
}

fn fixture() -> Fixture {
    fixture_with(MemoryStore::new())
}

fn fixture_with(store: MemoryStore) -> Fixture {
    let backend = RecordingBackend::new();
    let app = App::load(Box::new(backend.clone()), Box::new(store.clone())).unwrap();
    Fixture {
        app,
        backend,
        store,
    }
}

/// A project folder whose children named in `repositories` are git repositories.
fn project_dir(repositories: &[&str]) -> TempDir {
    let dir = TempDir::new().unwrap();
    for name in repositories {
        fs::create_dir_all(dir.path().join(name).join(".git")).unwrap();
    }
    dir
}

fn stored_session(project: &Project, title: &str, created_at: u64) -> Session {
    Session {
        id: SessionId::new(),
        project: project.id.clone(),
        scope: Scope::Project,
        cwd: project.root.clone(),
        title: title.into(),
        backend_session: Some(BackendSessionId::new(format!("backend-{title}"))),
        model: None,
        effort: None,
        status: Status::Idle,
        parent: None,
        auto_allow: false,
        instructions_injected: false,
        created_at,
    }
}

fn model_option() -> ConfigOption {
    ConfigOption {
        id: ConfigId::new("model"),
        name: "Model".into(),
        category: ConfigCategory::Model,
        current: ConfigValueId::new("fast"),
        choices: vec![ConfigChoice {
            id: ConfigValueId::new("fast"),
            name: "Fast".into(),
        }],
    }
}

fn new_session_request(backend: &RecordingBackend) -> (RequestId, SessionId, PathBuf) {
    let calls = backend.calls();
    let (request, session, cwd) = calls.last().unwrap().as_new_session().unwrap();
    (request, session.clone(), cwd.to_path_buf())
}

fn titles(nodes: &[SessionNode]) -> Vec<&str> {
    nodes.iter().map(|node| node.title.as_str()).collect()
}

fn session_status(app: &App, id: &SessionId) -> Status {
    app.sessions.iter().find(|s| &s.id == id).unwrap().status
}

#[test]
fn load_restores_projects_and_sessions_and_resets_live_statuses_to_idle() {
    let dir = project_dir(&["api"]);
    let project = discover_project(dir.path()).unwrap();
    let mut running = stored_session(&project, "running", 1);
    running.status = Status::Running;
    let mut waiting = stored_session(&project, "waiting", 2);
    waiting.status = Status::WaitingForYou;
    let mut done = stored_session(&project, "done", 3);
    done.status = Status::Done;
    let mut failed = stored_session(&project, "failed", 4);
    failed.status = Status::Failed;
    let store = MemoryStore::new();
    store.save_projects(std::slice::from_ref(&project)).unwrap();
    store
        .save_sessions(
            project.id.clone(),
            &[running.clone(), waiting, done, failed],
        )
        .unwrap();

    let fixture = fixture_with(store);

    let tree = fixture.app.tree();
    assert_eq!(tree.projects.len(), 1);
    assert_eq!(tree.projects[0].id, project.id);
    assert_eq!(tree.projects[0].sessions.len(), 4);
    assert!(
        tree.projects[0]
            .sessions
            .iter()
            .all(|node| node.status == Status::Idle)
    );
    let restored = fixture
        .app
        .sessions
        .iter()
        .find(|s| s.id == running.id)
        .unwrap();
    assert_eq!(restored.backend_session, running.backend_session);
    assert!(fixture.backend.calls().is_empty());
    assert_eq!(fixture.app.current_session(), None);
}

#[test]
fn opening_a_project_twice_returns_the_same_id_after_a_rescan() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let first = fixture.app.open_project(dir.path()).unwrap();
    fs::create_dir_all(dir.path().join("api/.git")).unwrap();

    let second = fixture.app.open_project(&dir.path().join(".")).unwrap();

    assert_eq!(first, second);
    let tree = fixture.app.tree();
    assert_eq!(tree.projects.len(), 1);
    assert_eq!(tree.projects[0].repositories.len(), 1);
    assert_eq!(fixture.store.load_projects().unwrap().len(), 1);
    assert_eq!(
        fixture.store.load_projects().unwrap()[0].repositories.len(),
        1
    );
}

#[test]
fn opening_projects_keeps_open_order_and_saves_them() {
    let first_dir = project_dir(&[]);
    let second_dir = project_dir(&[]);
    let mut fixture = fixture();

    let first = fixture.app.open_project(first_dir.path()).unwrap();
    let second = fixture.app.open_project(second_dir.path()).unwrap();

    assert_ne!(first, second);
    let ids: Vec<_> = fixture
        .app
        .tree()
        .projects
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(ids, vec![first.clone(), second.clone()]);
    let saved: Vec<_> = fixture
        .store
        .load_projects()
        .unwrap()
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(saved, vec![first, second]);
}

#[test]
fn opening_something_that_is_not_a_directory_fails() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();

    let result = fixture.app.open_project(&dir.path().join("missing"));

    assert!(matches!(
        result,
        Err(AppError::Project(ProjectError::NotADirectory { .. }))
    ));
    assert!(fixture.app.tree().projects.is_empty());
    assert!(fixture.store.load_projects().unwrap().is_empty());
}

#[test]
fn a_new_project_session_runs_in_the_root_and_a_repository_session_in_the_repository() {
    let dir = project_dir(&["api"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let repository = RepositoryId::try_from("api").unwrap();

    let in_project = fixture
        .app
        .new_session(project.clone(), Scope::Project)
        .unwrap();
    let (_, _, project_cwd) = new_session_request(&fixture.backend);
    let in_repository = fixture
        .app
        .new_session(project, Scope::Repository(repository.clone()))
        .unwrap();
    let (_, _, repository_cwd) = new_session_request(&fixture.backend);

    assert_eq!(project_cwd, dir.path());
    assert_eq!(repository_cwd, dir.path().join("api"));
    let stored = fixture.app.sessions.clone();
    let project_session = stored.iter().find(|s| s.id == in_project).unwrap();
    let repository_session = stored.iter().find(|s| s.id == in_repository).unwrap();
    assert_eq!(project_session.scope, Scope::Project);
    assert_eq!(project_session.cwd, dir.path());
    assert_eq!(repository_session.scope, Scope::Repository(repository));
    assert_eq!(repository_session.cwd, dir.path().join("api"));
}

#[test]
fn new_session_asks_the_backend_for_a_session_and_becomes_current() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();

    let first = fixture
        .app
        .new_session(project.clone(), Scope::Project)
        .unwrap();
    let (first_request, requested, cwd) = new_session_request(&fixture.backend);
    let second = fixture
        .app
        .new_session(project.clone(), Scope::Project)
        .unwrap();
    let (second_request, _, _) = new_session_request(&fixture.backend);

    assert_eq!(requested, first);
    assert_eq!(cwd, dir.path());
    assert_ne!(first_request, second_request);
    assert_eq!(fixture.backend.calls().len(), 2);
    assert_eq!(fixture.app.current_session(), Some(second.clone()));
    let session = fixture.app.sessions.iter().find(|s| s.id == first).unwrap();
    assert_eq!(session.title, "New session");
    assert_eq!(session.status, Status::Idle);
    assert_eq!(session.project, project);
    assert_eq!(session.backend_session, None);
    assert_eq!(session.parent, None);
    assert!(!session.auto_allow);
    assert!(!session.instructions_injected);
    assert!(session.created_at > 1_700_000_000);
    assert_eq!(
        fixture.store.load_sessions(project).unwrap(),
        fixture.app.sessions
    );
    assert!(fixture.app.pending.contains_key(&first_request));
    assert!(fixture.app.transcripts.contains_key(&first));
}

#[test]
fn new_session_rejects_unknown_projects_and_repositories() {
    let dir = project_dir(&["api"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let unknown_project = ProjectId::new();
    let unknown_repository = RepositoryId::try_from("ghost").unwrap();

    let no_project = fixture
        .app
        .new_session(unknown_project.clone(), Scope::Project);
    let no_repository = fixture
        .app
        .new_session(project, Scope::Repository(unknown_repository.clone()));

    assert!(matches!(no_project, Err(AppError::UnknownProject(id)) if id == unknown_project));
    assert!(
        matches!(no_repository, Err(AppError::UnknownRepository(id)) if id == unknown_repository)
    );
    assert!(fixture.backend.calls().is_empty());
    assert!(fixture.app.sessions.is_empty());
}

#[test]
fn a_backend_that_cannot_queue_the_create_leaves_no_session_behind() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let before = fixture
        .app
        .new_session(project.clone(), Scope::Project)
        .unwrap();
    fixture.backend.fail_next_call();

    let result = fixture.app.new_session(project.clone(), Scope::Project);

    assert!(matches!(
        result,
        Err(AppError::Backend(BackendError::Unavailable(_)))
    ));
    assert_eq!(fixture.app.current_session(), Some(before));
    assert_eq!(fixture.app.sessions.len(), 1);
    assert_eq!(fixture.app.pending.len(), 1);
    assert_eq!(fixture.store.load_sessions(project).unwrap().len(), 1);
}

#[test]
fn session_created_records_the_backend_id_and_config_options() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = fixture
        .app
        .new_session(project.clone(), Scope::Project)
        .unwrap();
    let (request, _, _) = new_session_request(&fixture.backend);

    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: session.clone(),
            backend_session: BackendSessionId::new("b-1"),
            config: vec![model_option()],
        })
        .unwrap();

    let stored = fixture.store.load_sessions(project).unwrap();
    assert_eq!(
        stored[0].backend_session,
        Some(BackendSessionId::new("b-1"))
    );
    assert_eq!(
        fixture.app.sessions[0].backend_session,
        Some(BackendSessionId::new("b-1"))
    );
    assert_eq!(fixture.app.config[&session], vec![model_option()]);
    assert!(fixture.app.pending.is_empty());
}

#[test]
fn session_loaded_remembers_the_config_options_and_clears_the_pending_request() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = fixture.app.new_session(project, Scope::Project).unwrap();
    let backend_session = BackendSessionId::new("kept");
    fixture.app.sessions[0].backend_session = Some(backend_session.clone());
    fixture.app.pending.clear();
    fixture
        .app
        .pending
        .insert(RequestId(99), (session.clone(), Request::Load));

    fixture
        .app
        .apply_event(BackendEvent::SessionLoaded {
            request: RequestId(99),
            session: session.clone(),
            config: vec![model_option()],
        })
        .unwrap();

    assert_eq!(fixture.app.config[&session], vec![model_option()]);
    assert!(fixture.app.pending.is_empty());
    assert_eq!(
        fixture.app.sessions[0].backend_session,
        Some(backend_session)
    );
}

#[test]
fn events_for_unknown_sessions_are_rejected() {
    let mut fixture = fixture();
    let ghost = SessionId::new();
    let events = [
        BackendEvent::SessionCreated {
            request: RequestId(1),
            session: ghost.clone(),
            backend_session: BackendSessionId::new("b"),
            config: Vec::new(),
        },
        BackendEvent::SessionLoaded {
            request: RequestId(1),
            session: ghost.clone(),
            config: Vec::new(),
        },
        BackendEvent::Update {
            session: ghost.clone(),
            update: Update::UserText("hi".into()),
        },
        BackendEvent::RequestFailed {
            request: RequestId(1),
            session: ghost.clone(),
            error: "boom".into(),
        },
    ];

    for event in events {
        let result = fixture.app.apply_event(event);
        assert!(matches!(result, Err(AppError::UnknownSession(id)) if id == ghost));
    }

    assert!(fixture.app.config.is_empty());
    assert!(fixture.store.load_updates(ghost).unwrap().is_empty());
}

#[test]
fn a_failed_create_marks_the_session_failed_with_a_notice() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = fixture
        .app
        .new_session(project.clone(), Scope::Project)
        .unwrap();
    let (request, _, _) = new_session_request(&fixture.backend);

    fixture
        .app
        .apply_event(BackendEvent::RequestFailed {
            request,
            session: session.clone(),
            error: "agent did not start".into(),
        })
        .unwrap();

    assert_eq!(session_status(&fixture.app, &session), Status::Failed);
    assert_eq!(
        fixture.store.load_sessions(project).unwrap()[0].status,
        Status::Failed
    );
    assert_eq!(
        fixture.app.transcripts[&session].entries(),
        [Entry::Notice("agent did not start".into())]
    );
    assert_eq!(
        fixture.store.load_updates(session).unwrap(),
        vec![Update::Notice("agent did not start".into())]
    );
    assert!(fixture.app.pending.is_empty());
}

#[test]
fn updates_are_persisted_and_applied_to_a_loaded_transcript() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = fixture.app.new_session(project, Scope::Project).unwrap();

    fixture
        .app
        .apply_event(BackendEvent::Update {
            session: session.clone(),
            update: Update::AgentText("hello".into()),
        })
        .unwrap();

    assert_eq!(
        fixture.app.transcripts[&session].entries(),
        [Entry::Agent("hello".into())]
    );
    assert_eq!(
        fixture.store.load_updates(session).unwrap(),
        vec![Update::AgentText("hello".into())]
    );
}

#[test]
fn updates_for_a_session_not_yet_opened_are_only_persisted() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let stored = stored_session(&project, "old", 1);
    let store = MemoryStore::new();
    store.save_projects(std::slice::from_ref(&project)).unwrap();
    store
        .save_sessions(project.id, std::slice::from_ref(&stored))
        .unwrap();
    let mut fixture = fixture_with(store);

    fixture
        .app
        .apply_event(BackendEvent::Update {
            session: stored.id.clone(),
            update: Update::AgentText("late".into()),
        })
        .unwrap();
    assert!(!fixture.app.transcripts.contains_key(&stored.id));
    fixture.app.select_session(stored.id.clone()).unwrap();

    assert_eq!(
        fixture.app.transcripts[&stored.id].entries(),
        [Entry::Agent("late".into())]
    );
}

#[test]
fn the_tree_lists_sessions_newest_first_under_their_project() {
    let dir = project_dir(&["api", "web"]);
    let other_dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let other = discover_project(other_dir.path()).unwrap();
    let oldest = stored_session(&project, "oldest", 10);
    let newest = stored_session(&project, "newest", 30);
    let middle = stored_session(&project, "middle", 20);
    let tie_first = stored_session(&project, "tie-first", 30);
    let mut api = stored_session(&project, "api", 5);
    api.scope = Scope::Repository(RepositoryId::try_from("api").unwrap());
    let elsewhere = stored_session(&other, "elsewhere", 99);
    let store = MemoryStore::new();
    store
        .save_projects(&[project.clone(), other.clone()])
        .unwrap();
    store
        .save_sessions(
            project.id.clone(),
            &[oldest, newest, middle, tie_first, api],
        )
        .unwrap();
    store.save_sessions(other.id.clone(), &[elsewhere]).unwrap();

    let tree = fixture_with(store).app.tree();

    let first = &tree.projects[0];
    assert_eq!(first.name, project.name);
    assert_eq!(first.root, project.root);
    assert!(!first.has_global_instructions);
    assert_eq!(
        titles(&first.sessions),
        ["tie-first", "newest", "middle", "oldest", "api"]
    );
    assert_eq!(first.sessions[0].scope_label, project.name);
    assert_eq!(first.sessions[4].scope_label, "api");
    assert_eq!(first.repositories.len(), 2);
    assert_eq!(first.repositories[0].name, "api");
    assert_eq!(
        first.repositories[0].id,
        RepositoryId::try_from("api").unwrap()
    );
    assert!(!first.repositories[0].has_instructions);
    assert_eq!(titles(&tree.projects[1].sessions), ["elsewhere"]);
}

#[test]
fn the_tree_flags_instruction_files_and_nests_children_under_their_parent() {
    let dir = project_dir(&["api"]);
    fs::write(dir.path().join("AGENTS.md"), "rules").unwrap();
    fs::write(dir.path().join("api/AGENTS.md"), "rules").unwrap();
    let project = discover_project(dir.path()).unwrap();
    let parent = stored_session(&project, "parent", 10);
    let mut older_child = stored_session(&project, "older child", 11);
    older_child.parent = Some(parent.id.clone());
    let mut newer_child = stored_session(&project, "newer child", 12);
    newer_child.parent = Some(parent.id.clone());
    let mut grandchild = stored_session(&project, "grandchild", 13);
    grandchild.parent = Some(newer_child.id.clone());
    let store = MemoryStore::new();
    store.save_projects(std::slice::from_ref(&project)).unwrap();
    store
        .save_sessions(project.id, &[parent, older_child, newer_child, grandchild])
        .unwrap();

    let tree = fixture_with(store).app.tree();

    let node = &tree.projects[0];
    assert!(node.has_global_instructions);
    assert!(node.repositories[0].has_instructions);
    assert_eq!(titles(&node.sessions), ["parent"]);
    let children = &node.sessions[0].children;
    assert_eq!(titles(children), ["newer child", "older child"]);
    assert_eq!(titles(&children[0].children), ["grandchild"]);
    assert!(children[1].children.is_empty());
}

#[test]
fn a_session_of_a_vanished_repository_is_labelled_with_the_repository_id() {
    let dir = project_dir(&["api"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    fixture
        .app
        .new_session(
            project.clone(),
            Scope::Repository(RepositoryId::try_from("api").unwrap()),
        )
        .unwrap();
    fs::remove_dir_all(dir.path().join("api/.git")).unwrap();
    fixture.app.open_project(dir.path()).unwrap();

    let tree = fixture.app.tree();

    assert!(tree.projects[0].repositories.is_empty());
    assert_eq!(tree.projects[0].sessions[0].scope_label, "api");
}

#[test]
fn selecting_a_session_loads_its_transcript_once() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let first = stored_session(&project, "first", 1);
    let second = stored_session(&project, "second", 2);
    let store = MemoryStore::new();
    store.save_projects(std::slice::from_ref(&project)).unwrap();
    store
        .save_sessions(project.id, &[first.clone(), second.clone()])
        .unwrap();
    store
        .append_update(first.id.clone(), &Update::UserText("hi".into()))
        .unwrap();
    let mut fixture = fixture_with(store);

    fixture.app.select_session(first.id.clone()).unwrap();
    fixture.app.select_session(second.id.clone()).unwrap();
    fixture.app.select_session(first.id.clone()).unwrap();

    assert_eq!(fixture.store.update_loads(), 2);
    assert_eq!(fixture.app.current_session(), Some(first.id.clone()));
    assert_eq!(
        fixture.app.transcripts[&first.id].entries(),
        [Entry::User("hi".into())]
    );
    assert!(fixture.app.transcripts[&second.id].entries().is_empty());
}

#[test]
fn selecting_an_unknown_session_or_one_with_a_corrupt_log_keeps_the_current_session() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let good = stored_session(&project, "good", 1);
    let bad = stored_session(&project, "bad", 2);
    let store = MemoryStore::new();
    store.save_projects(std::slice::from_ref(&project)).unwrap();
    store
        .save_sessions(project.id, &[good.clone(), bad.clone()])
        .unwrap();
    let mut fixture = fixture_with(store);
    fixture.app.select_session(good.id.clone()).unwrap();
    fixture.store.set_corrupt_updates(true);

    let unknown = fixture.app.select_session(SessionId::new());
    let corrupt = fixture.app.select_session(bad.id.clone());

    assert!(matches!(unknown, Err(AppError::UnknownSession(_))));
    assert!(matches!(
        corrupt,
        Err(AppError::Store(StoreError::Corrupt { .. }))
    ));
    assert_eq!(fixture.app.current_session(), Some(good.id));
    assert!(!fixture.app.transcripts.contains_key(&bad.id));
}

#[test]
fn closing_a_project_forgets_its_sessions_but_keeps_the_others() {
    let dir = project_dir(&[]);
    let other_dir = project_dir(&[]);
    let mut fixture = fixture();
    let closed = fixture.app.open_project(dir.path()).unwrap();
    let kept = fixture.app.open_project(other_dir.path()).unwrap();
    let closed_session = fixture
        .app
        .new_session(closed.clone(), Scope::Project)
        .unwrap();
    let kept_session = fixture
        .app
        .new_session(kept.clone(), Scope::Project)
        .unwrap();
    fixture.app.select_session(closed_session.clone()).unwrap();
    fixture
        .app
        .config
        .insert(closed_session.clone(), vec![model_option()]);
    fixture
        .app
        .config
        .insert(kept_session.clone(), vec![model_option()]);

    fixture.app.close_project(closed.clone()).unwrap();

    let tree = fixture.app.tree();
    assert_eq!(tree.projects.len(), 1);
    assert_eq!(tree.projects[0].id, kept);
    assert_eq!(tree.projects[0].sessions.len(), 1);
    assert_eq!(fixture.app.current_session(), None);
    assert!(!fixture.app.transcripts.contains_key(&closed_session));
    assert!(fixture.app.transcripts.contains_key(&kept_session));
    assert!(!fixture.app.config.contains_key(&closed_session));
    assert!(fixture.app.config.contains_key(&kept_session));
    assert_eq!(
        fixture
            .app
            .pending
            .values()
            .map(|(session, _)| session)
            .collect::<Vec<_>>(),
        [&kept_session]
    );
    let saved = fixture.store.load_projects().unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].id, kept);
    assert_eq!(fixture.store.load_sessions(closed).unwrap().len(), 1);
    assert_eq!(fixture.store.load_sessions(kept).unwrap().len(), 1);
}

#[test]
fn closing_a_project_keeps_the_current_session_of_another_project() {
    let dir = project_dir(&[]);
    let other_dir = project_dir(&[]);
    let mut fixture = fixture();
    let closed = fixture.app.open_project(dir.path()).unwrap();
    let kept = fixture.app.open_project(other_dir.path()).unwrap();
    fixture
        .app
        .new_session(closed.clone(), Scope::Project)
        .unwrap();
    let kept_session = fixture.app.new_session(kept, Scope::Project).unwrap();

    fixture.app.close_project(closed).unwrap();

    assert_eq!(fixture.app.current_session(), Some(kept_session));
}

#[test]
fn closing_an_unknown_project_fails() {
    let mut fixture = fixture();
    let ghost = ProjectId::new();

    let result = fixture.app.close_project(ghost.clone());

    assert!(matches!(result, Err(AppError::UnknownProject(id)) if id == ghost));
}

#[test]
fn title_from_prompt_takes_the_first_line_and_cuts_at_sixty() {
    let sixty = "a".repeat(60);
    let long = "é".repeat(61);

    assert_eq!(title_from_prompt("Fix the build"), "Fix the build");
    assert_eq!(
        title_from_prompt("\n \t\n  Fix it  \nthen deploy"),
        "Fix it"
    );
    assert_eq!(title_from_prompt(&sixty), sixty);
    assert_eq!(title_from_prompt(&long), format!("{}…", "é".repeat(60)));
    assert_eq!(title_from_prompt(""), "");
    assert_eq!(title_from_prompt(" \n \n"), "");
}

// Prompting, permissions, config and the session and env view-models. These tests go through
// the public surface only: methods, view-models, the fakes and the store.

const RESUMED: &str = "resumed without the agent's memory of earlier turns";

fn store_with(project: &Project, sessions: &[Session]) -> MemoryStore {
    let store = MemoryStore::new();
    store.save_projects(std::slice::from_ref(project)).unwrap();
    store.save_sessions(project.id.clone(), sessions).unwrap();
    store
}

/// Starts a session in `project` and lets the backend create it with `config`.
fn created_session(
    fixture: &mut Fixture,
    project: &ProjectId,
    scope: Scope,
    config: Vec<ConfigOption>,
) -> SessionId {
    let session = fixture.app.new_session(project.clone(), scope).unwrap();
    let (request, _, _) = new_session_request(&fixture.backend);
    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: session.clone(),
            backend_session: BackendSessionId::new("b-1"),
            config,
        })
        .unwrap();
    session
}

fn send(fixture: &mut Fixture, session: &SessionId, update: Update) {
    fixture
        .app
        .apply_event(BackendEvent::Update {
            session: session.clone(),
            update,
        })
        .unwrap();
}

fn last_call(backend: &RecordingBackend) -> Call {
    backend.calls().last().unwrap().clone()
}

fn prompt_call(session: &SessionId, text: &str) -> Call {
    Call::Prompt {
        session: session.clone(),
        content: vec![PromptContent::Text(text.into())],
    }
}

/// The request id of the last `load_session` call.
fn load_request(backend: &RecordingBackend) -> RequestId {
    match last_call(backend) {
        Call::LoadSession { request, .. } => request,
        other => panic!("expected a load, got {other:?}"),
    }
}

fn status(app: &App) -> Status {
    app.session_view().unwrap().status
}

fn entries(app: &App) -> Vec<Entry> {
    app.session_view().unwrap().entries.to_vec()
}

fn saved(store: &MemoryStore, project: &ProjectId, session: &SessionId) -> Session {
    store
        .load_sessions(project.clone())
        .unwrap()
        .into_iter()
        .find(|saved| &saved.id == session)
        .unwrap()
}

fn choice(id: &str, name: &str) -> ConfigChoice {
    ConfigChoice {
        id: ConfigValueId::new(id),
        name: name.into(),
    }
}

fn option(
    id: &str,
    category: ConfigCategory,
    current: &str,
    choices: &[(&str, &str)],
) -> ConfigOption {
    ConfigOption {
        id: ConfigId::new(id),
        name: id.into(),
        category,
        current: ConfigValueId::new(current),
        choices: choices.iter().map(|(id, name)| choice(id, name)).collect(),
    }
}

fn permission(id: u64, options: &[(&str, PermissionKind)]) -> Update {
    Update::PermissionRequested {
        id: PermissionId(id),
        tool_call_id: ToolCallId::new("t-1"),
        title: "Edit main.rs".into(),
        kind: ToolKind::Edit,
        options: options
            .iter()
            .map(|(id, kind)| PermissionOption {
                id: PermissionOptionId::new(*id),
                name: (*id).into(),
                kind: *kind,
            })
            .collect(),
    }
}

/// The option a settled permission entry was answered with, `None` when it was cancelled.
fn answer_of(entry: &Entry) -> Option<PermissionOptionId> {
    match entry {
        Entry::Permission {
            answer: Some(option),
            ..
        } => option.clone(),
        other => panic!("expected a settled permission, got {other:?}"),
    }
}

fn answered_updates(store: &MemoryStore, session: &SessionId) -> Vec<Update> {
    store
        .load_updates(session.clone())
        .unwrap()
        .into_iter()
        .filter(|update| matches!(update, Update::PermissionAnswered { .. }))
        .collect()
}

#[test]
fn the_first_prompt_injects_instructions_and_sets_the_title() {
    let dir = project_dir(&["api"]);
    fs::write(dir.path().join("AGENTS.md"), [0xff, 0xfe]).unwrap();
    fs::write(dir.path().join("api/AGENTS.md"), "api rules").unwrap();
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let api = RepositoryId::try_from("api").unwrap();
    let session = created_session(&mut fixture, &project, Scope::Repository(api), Vec::new());
    let discovered = discover_project(dir.path()).unwrap();
    let chain = instruction_chain(&discovered, discovered.repositories.first());
    assert_eq!(chain.files.len(), 1);
    let skipped = &chain.skipped[0];
    let notice = format!(
        "instruction file skipped: {}: {}",
        skipped.path.display(),
        skipped.reason
    );

    fixture
        .app
        .submit_prompt("Fix the build\nnow", Vec::new())
        .unwrap();

    assert_eq!(
        last_call(&fixture.backend),
        prompt_call(&session, &inject(&chain, "Fix the build\nnow"))
    );
    let view = fixture.app.session_view().unwrap();
    assert_eq!(view.breadcrumb.last().unwrap(), "Fix the build");
    assert_eq!(view.status, Status::Running);
    assert_eq!(
        view.entries,
        [
            Entry::User("Fix the build\nnow".into()),
            Entry::Notice(notice.clone())
        ]
    );
    assert_eq!(
        fixture.store.load_updates(session.clone()).unwrap(),
        vec![
            Update::UserText("Fix the build\nnow".into()),
            Update::Notice(notice)
        ]
    );
    let saved = saved(&fixture.store, &project, &session);
    assert!(saved.instructions_injected);
    assert_eq!(saved.title, "Fix the build");
    assert_eq!(saved.status, Status::Running);
}

#[test]
fn later_prompts_are_sent_without_instructions() {
    let dir = project_dir(&[]);
    fs::write(dir.path().join("AGENTS.md"), "rules").unwrap();
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());

    fixture.app.submit_prompt("one", Vec::new()).unwrap();
    send(
        &mut fixture,
        &session,
        Update::TurnEnded(StopReason::EndTurn),
    );
    fixture.app.submit_prompt("two", Vec::new()).unwrap();

    assert_eq!(last_call(&fixture.backend), prompt_call(&session, "two"));
    let view = fixture.app.session_view().unwrap();
    assert_eq!(view.breadcrumb.last().unwrap(), "one");
    assert_eq!(
        view.entries,
        [
            Entry::User("one".into()),
            Entry::TurnEnded(StopReason::EndTurn),
            Entry::User("two".into())
        ]
    );
}

#[test]
fn a_prompt_on_a_stored_session_loads_it_first_and_sends_after_loaded() {
    let dir = project_dir(&[]);
    fs::write(dir.path().join("AGENTS.md"), "rules").unwrap();
    let project = discover_project(dir.path()).unwrap();
    let mut old = stored_session(&project, "old", 1);
    old.instructions_injected = true;
    let mut fixture = fixture_with(store_with(&project, std::slice::from_ref(&old)));
    fixture.app.select_session(old.id.clone()).unwrap();

    fixture.app.submit_prompt("again", Vec::new()).unwrap();

    let request = load_request(&fixture.backend);
    assert_eq!(
        fixture.backend.calls(),
        [Call::LoadSession {
            request,
            session: old.id.clone(),
            backend_session: old.backend_session.clone().unwrap(),
            cwd: project.root.clone(),
        }]
    );
    assert_eq!(status(&fixture.app), Status::Running);
    assert_eq!(entries(&fixture.app), [Entry::User("again".into())]);

    fixture
        .app
        .apply_event(BackendEvent::SessionLoaded {
            request,
            session: old.id.clone(),
            config: Vec::new(),
        })
        .unwrap();
    assert_eq!(last_call(&fixture.backend), prompt_call(&old.id, "again"));

    fixture.app.submit_prompt("more", Vec::new()).unwrap();
    assert_eq!(fixture.backend.calls().len(), 3);
    assert_eq!(last_call(&fixture.backend), prompt_call(&old.id, "more"));
    assert_eq!(saved(&fixture.store, &project.id, &old.id).title, "old");
}

#[test]
fn a_failed_load_falls_back_to_a_new_session_and_sends_the_queued_prompt() {
    let dir = project_dir(&[]);
    fs::write(dir.path().join("AGENTS.md"), "rules").unwrap();
    let project = discover_project(dir.path()).unwrap();
    let mut old = stored_session(&project, "old", 1);
    old.instructions_injected = true;
    let store = store_with(&project, std::slice::from_ref(&old));
    store
        .append_update(old.id.clone(), &Update::UserText("before".into()))
        .unwrap();
    store
        .append_update(old.id.clone(), &Update::AgentText("earlier answer".into()))
        .unwrap();
    let mut fixture = fixture_with(store);
    fixture.app.select_session(old.id.clone()).unwrap();
    fixture.app.submit_prompt("again", Vec::new()).unwrap();

    fixture
        .app
        .apply_event(BackendEvent::RequestFailed {
            request: load_request(&fixture.backend),
            session: old.id.clone(),
            error: "session not found".into(),
        })
        .unwrap();

    let (request, created, cwd) = new_session_request(&fixture.backend);
    assert_eq!(created, old.id);
    assert_eq!(cwd, project.root);
    assert_eq!(fixture.backend.calls().len(), 2);
    assert_eq!(status(&fixture.app), Status::Running);
    assert!(!saved(&fixture.store, &project.id, &old.id).instructions_injected);

    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: old.id.clone(),
            backend_session: BackendSessionId::new("fresh"),
            config: Vec::new(),
        })
        .unwrap();

    let chain = instruction_chain(&project, None);
    assert_eq!(chain.files.len(), 1);
    assert_eq!(
        last_call(&fixture.backend),
        prompt_call(&old.id, &inject(&chain, "again"))
    );
    assert_eq!(
        entries(&fixture.app),
        [
            Entry::User("before".into()),
            Entry::Agent("earlier answer".into()),
            Entry::User("again".into()),
            Entry::Notice(RESUMED.into())
        ]
    );
    assert_eq!(
        fixture.store.load_updates(old.id.clone()).unwrap().last(),
        Some(&Update::Notice(RESUMED.into()))
    );
    let saved = saved(&fixture.store, &project.id, &old.id);
    assert_eq!(saved.backend_session, Some(BackendSessionId::new("fresh")));
    assert!(saved.instructions_injected);
}

#[test]
fn a_failed_create_drops_the_queued_prompt_and_a_retry_creates_again() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = fixture.app.new_session(project, Scope::Project).unwrap();
    let (request, _, _) = new_session_request(&fixture.backend);
    fixture.app.submit_prompt("lost", Vec::new()).unwrap();
    assert_eq!(fixture.backend.calls().len(), 1);

    fixture
        .app
        .apply_event(BackendEvent::RequestFailed {
            request,
            session: session.clone(),
            error: "agent did not start".into(),
        })
        .unwrap();
    assert_eq!(status(&fixture.app), Status::Failed);
    fixture.app.submit_prompt("retry", Vec::new()).unwrap();
    let (retry, _, _) = new_session_request(&fixture.backend);
    assert_eq!(status(&fixture.app), Status::Running);
    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request: retry,
            session: session.clone(),
            backend_session: BackendSessionId::new("b-2"),
            config: Vec::new(),
        })
        .unwrap();

    assert_eq!(fixture.backend.calls().len(), 3);
    assert_eq!(last_call(&fixture.backend), prompt_call(&session, "retry"));
}

#[test]
fn an_empty_prompt_is_a_no_op() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());

    fixture.app.submit_prompt("", Vec::new()).unwrap();
    fixture.app.submit_prompt(" \n\t", Vec::new()).unwrap();

    assert_eq!(fixture.backend.calls().len(), 1);
    let view = fixture.app.session_view().unwrap();
    assert!(view.entries.is_empty());
    assert_eq!(view.status, Status::Idle);
    assert_eq!(view.breadcrumb.last().unwrap(), "New session");
    assert!(fixture.store.load_updates(session).unwrap().is_empty());
}

#[test]
fn prompting_or_cancelling_without_a_selected_session_fails() {
    let mut fixture = fixture();

    let prompt = fixture.app.submit_prompt("hi", Vec::new());
    let cancel = fixture.app.cancel();

    assert!(matches!(prompt, Err(AppError::NoCurrentSession)));
    assert!(matches!(cancel, Err(AppError::NoCurrentSession)));
    assert!(fixture.backend.calls().is_empty());
}

#[test]
fn image_attachments_become_images_and_others_file_links() {
    let dir = project_dir(&[]);
    let files = TempDir::new().unwrap();
    let names = ["a.PNG", "b.jpg", "c.Jpeg", "d.gif", "e.webp"];
    for name in names {
        fs::write(files.path().join(name), "hi").unwrap();
    }
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    let mut attachments: Vec<Attachment> = names
        .iter()
        .map(|name| Attachment {
            path: files.path().join(name),
        })
        .collect();
    attachments.push(Attachment {
        path: files.path().join("notes.txt"),
    });
    attachments.push(Attachment {
        path: files.path().join("png"),
    });

    fixture.app.submit_prompt("", attachments).unwrap();

    let image = |mime: &str| PromptContent::Image {
        mime: mime.into(),
        base64: "aGk=".into(),
    };
    assert_eq!(
        last_call(&fixture.backend),
        Call::Prompt {
            session,
            content: vec![
                image("image/png"),
                image("image/jpeg"),
                image("image/jpeg"),
                image("image/gif"),
                image("image/webp"),
                PromptContent::FileLink {
                    path: files.path().join("notes.txt")
                },
                PromptContent::FileLink {
                    path: files.path().join("png")
                },
            ],
        }
    );
    let view = fixture.app.session_view().unwrap();
    assert_eq!(view.breadcrumb.last().unwrap(), "New session");
    assert_eq!(view.status, Status::Running);
}

#[test]
fn an_unreadable_image_attachment_fails_and_sends_nothing() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    let missing = dir.path().join("missing.png");

    let result = fixture.app.submit_prompt(
        "look",
        vec![Attachment {
            path: missing.clone(),
        }],
    );

    assert!(matches!(result, Err(AppError::Attachment { path, .. }) if path == missing));
    assert_eq!(fixture.backend.calls().len(), 1);
    let view = fixture.app.session_view().unwrap();
    assert!(view.entries.is_empty());
    assert_eq!(view.status, Status::Idle);
    assert_eq!(view.breadcrumb.last().unwrap(), "New session");
    assert!(fixture.store.load_updates(session).unwrap().is_empty());
}

#[test]
fn a_permission_request_sets_waiting_and_an_answer_sets_running() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    let options = [
        ("allow", PermissionKind::AllowOnce),
        ("reject", PermissionKind::RejectOnce),
    ];
    let allow = PermissionOptionId::new("allow");
    fixture.app.submit_prompt("go", Vec::new()).unwrap();

    send(&mut fixture, &session, permission(1, &options));
    send(&mut fixture, &session, permission(2, &options));
    assert_eq!(status(&fixture.app), Status::WaitingForYou);
    assert_eq!(
        saved(&fixture.store, &project, &session).status,
        Status::WaitingForYou
    );

    fixture
        .app
        .answer_permission(session.clone(), PermissionId(1), Some(allow.clone()))
        .unwrap();
    assert_eq!(
        last_call(&fixture.backend),
        Call::RespondPermission {
            session: session.clone(),
            permission: PermissionId(1),
            option: Some(allow.clone()),
        }
    );
    assert_eq!(status(&fixture.app), Status::WaitingForYou);

    fixture
        .app
        .answer_permission(session.clone(), PermissionId(2), None)
        .unwrap();
    assert_eq!(
        last_call(&fixture.backend),
        Call::RespondPermission {
            session: session.clone(),
            permission: PermissionId(2),
            option: None,
        }
    );
    assert_eq!(status(&fixture.app), Status::Running);
    assert_eq!(
        saved(&fixture.store, &project, &session).status,
        Status::Running
    );
    let entries = entries(&fixture.app);
    assert_eq!(answer_of(&entries[1]), Some(allow.clone()));
    assert_eq!(answer_of(&entries[2]), None);
    assert_eq!(
        answered_updates(&fixture.store, &session),
        [
            Update::PermissionAnswered {
                id: PermissionId(1),
                option: Some(allow)
            },
            Update::PermissionAnswered {
                id: PermissionId(2),
                option: None
            }
        ]
    );
}

#[test]
fn auto_allow_answers_with_allow_once_without_waiting() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.set_auto_allow(session.clone(), true).unwrap();
    assert!(fixture.app.session_view().unwrap().auto_allow);
    assert!(saved(&fixture.store, &project, &session).auto_allow);
    fixture.app.submit_prompt("go", Vec::new()).unwrap();

    send(
        &mut fixture,
        &session,
        permission(
            1,
            &[
                ("reject", PermissionKind::RejectOnce),
                ("always", PermissionKind::AllowAlways),
                ("once", PermissionKind::AllowOnce),
            ],
        ),
    );
    assert_eq!(
        last_call(&fixture.backend),
        Call::RespondPermission {
            session: session.clone(),
            permission: PermissionId(1),
            option: Some(PermissionOptionId::new("once")),
        }
    );
    assert_eq!(status(&fixture.app), Status::Running);
    assert_eq!(
        answer_of(&entries(&fixture.app)[1]),
        Some(PermissionOptionId::new("once"))
    );

    send(
        &mut fixture,
        &session,
        permission(
            2,
            &[
                ("reject", PermissionKind::RejectAlways),
                ("always", PermissionKind::AllowAlways),
            ],
        ),
    );
    assert_eq!(
        last_call(&fixture.backend),
        Call::RespondPermission {
            session: session.clone(),
            permission: PermissionId(2),
            option: Some(PermissionOptionId::new("always")),
        }
    );
    assert_eq!(answered_updates(&fixture.store, &session).len(), 2);

    let calls = fixture.backend.calls().len();
    send(
        &mut fixture,
        &session,
        permission(3, &[("reject", PermissionKind::RejectOnce)]),
    );
    assert_eq!(fixture.backend.calls().len(), calls);
    assert_eq!(status(&fixture.app), Status::WaitingForYou);

    fixture.app.set_auto_allow(session.clone(), false).unwrap();
    assert!(!fixture.app.session_view().unwrap().auto_allow);
    assert!(matches!(
        fixture.app.set_auto_allow(SessionId::new(), true),
        Err(AppError::UnknownSession(_))
    ));
}

#[test]
fn a_late_permission_answer_is_ignored() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    let options = [("allow", PermissionKind::AllowOnce)];
    let allow = || Some(PermissionOptionId::new("allow"));
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    send(&mut fixture, &session, permission(1, &options));
    send(
        &mut fixture,
        &session,
        Update::TurnEnded(StopReason::Cancelled),
    );
    fixture.app.submit_prompt("again", Vec::new()).unwrap();
    send(&mut fixture, &session, permission(2, &options));
    send(&mut fixture, &session, Update::Failed("crashed".into()));
    fixture.app.submit_prompt("once more", Vec::new()).unwrap();
    send(&mut fixture, &session, permission(3, &options));
    fixture
        .app
        .answer_permission(session.clone(), PermissionId(3), allow())
        .unwrap();
    let calls = fixture.backend.calls().len();

    for id in [1, 2, 3] {
        fixture
            .app
            .answer_permission(session.clone(), PermissionId(id), allow())
            .unwrap();
    }
    fixture
        .app
        .answer_permission(SessionId::new(), PermissionId(3), allow())
        .unwrap();

    assert_eq!(fixture.backend.calls().len(), calls);
    assert_eq!(status(&fixture.app), Status::Running);
    assert_eq!(
        answered_updates(&fixture.store, &session),
        [Update::PermissionAnswered {
            id: PermissionId(3),
            option: allow()
        }]
    );
}

#[test]
fn turn_ended_sets_done_and_failed_sets_failed() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    assert!(fixture.app.session_view().unwrap().running);

    send(
        &mut fixture,
        &session,
        Update::TurnEnded(StopReason::EndTurn),
    );
    assert_eq!(status(&fixture.app), Status::Done);
    assert!(!fixture.app.session_view().unwrap().running);
    assert_eq!(
        saved(&fixture.store, &project, &session).status,
        Status::Done
    );

    fixture.app.submit_prompt("again", Vec::new()).unwrap();
    send(&mut fixture, &session, Update::Failed("crashed".into()));
    assert_eq!(status(&fixture.app), Status::Failed);
    assert_eq!(
        saved(&fixture.store, &project, &session).status,
        Status::Failed
    );
    assert_eq!(
        entries(&fixture.app).last(),
        Some(&Entry::Failed("crashed".into()))
    );
}

#[test]
fn cancel_asks_the_backend_and_the_turn_end_settles_the_status() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();

    fixture.app.cancel().unwrap();
    assert_eq!(
        last_call(&fixture.backend),
        Call::Cancel {
            session: session.clone()
        }
    );
    assert_eq!(status(&fixture.app), Status::Running);

    send(
        &mut fixture,
        &session,
        Update::TurnEnded(StopReason::Cancelled),
    );
    assert_eq!(status(&fixture.app), Status::Done);
}

#[test]
fn cancelling_a_queued_prompt_drops_it() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let old = stored_session(&project, "old", 1);
    let mut fixture = fixture_with(store_with(&project, std::slice::from_ref(&old)));
    fixture.app.select_session(old.id.clone()).unwrap();
    fixture.app.submit_prompt("again", Vec::new()).unwrap();
    let request = load_request(&fixture.backend);

    fixture.app.cancel().unwrap();
    assert_eq!(status(&fixture.app), Status::Idle);
    assert_eq!(
        saved(&fixture.store, &project.id, &old.id).status,
        Status::Idle
    );
    fixture
        .app
        .apply_event(BackendEvent::SessionLoaded {
            request,
            session: old.id.clone(),
            config: Vec::new(),
        })
        .unwrap();

    assert_eq!(fixture.backend.calls().len(), 1);
}

#[test]
fn set_model_sends_the_model_config_option() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let models = [("fast", "Fast"), ("opus", "Opus")];
    let efforts = [("low", "Low"), ("high", "High")];
    let session = created_session(
        &mut fixture,
        &project,
        Scope::Project,
        vec![
            option("mode", ConfigCategory::Mode, "default", &[]),
            option("model", ConfigCategory::Model, "fast", &models),
        ],
    );

    fixture
        .app
        .set_model(session.clone(), ConfigValueId::new("opus"))
        .unwrap();
    assert_eq!(
        last_call(&fixture.backend),
        Call::SetConfig {
            session: session.clone(),
            option: ConfigId::new("model"),
            value: ConfigValueId::new("opus"),
        }
    );
    assert_eq!(
        saved(&fixture.store, &project, &session).model,
        Some(ConfigValueId::new("opus"))
    );
    assert_eq!(
        fixture
            .app
            .session_view()
            .unwrap()
            .model
            .unwrap()
            .current_name,
        "Opus"
    );

    let calls = fixture.backend.calls().len();
    let no_effort = fixture
        .app
        .set_effort(session.clone(), ConfigValueId::new("high"));
    assert!(matches!(
        no_effort,
        Err(AppError::NoSuchConfig(ConfigCategory::Effort))
    ));
    assert_eq!(fixture.backend.calls().len(), calls);
    assert!(matches!(
        fixture
            .app
            .set_model(SessionId::new(), ConfigValueId::new("opus")),
        Err(AppError::UnknownSession(_))
    ));

    send(
        &mut fixture,
        &session,
        Update::ConfigOptions(vec![
            option("effort", ConfigCategory::Effort, "low", &efforts),
            option("model", ConfigCategory::Model, "fast", &models),
        ]),
    );
    let synced = saved(&fixture.store, &project, &session);
    assert_eq!(synced.model, Some(ConfigValueId::new("fast")));
    assert_eq!(synced.effort, Some(ConfigValueId::new("low")));

    fixture
        .app
        .set_effort(session.clone(), ConfigValueId::new("high"))
        .unwrap();
    assert_eq!(
        last_call(&fixture.backend),
        Call::SetConfig {
            session: session.clone(),
            option: ConfigId::new("effort"),
            value: ConfigValueId::new("high"),
        }
    );
    let view = fixture.app.session_view().unwrap();
    assert_eq!(view.effort.unwrap().current, ConfigValueId::new("high"));
    assert_eq!(view.model.unwrap().current, ConfigValueId::new("fast"));
    assert_eq!(
        saved(&fixture.store, &project, &session).effort,
        Some(ConfigValueId::new("high"))
    );
}

#[test]
fn the_session_view_borrows_entries_and_carries_choices() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let parent = stored_session(&project, "parent", 1);
    let mut child = stored_session(&project, "child", 2);
    child.parent = Some(parent.id.clone());
    let mut fixture = fixture_with(store_with(&project, &[parent.clone(), child.clone()]));
    assert!(fixture.app.session_view().is_none());
    fixture.app.select_session(child.id.clone()).unwrap();
    fixture.app.submit_prompt("hi", Vec::new()).unwrap();
    let models = [("fast", "Fast"), ("opus", "Opus")];
    let efforts = [("low", "Low"), ("high", "High")];
    fixture
        .app
        .apply_event(BackendEvent::SessionLoaded {
            request: load_request(&fixture.backend),
            session: child.id.clone(),
            config: vec![
                option("model", ConfigCategory::Model, "fast", &models),
                option("effort", ConfigCategory::Effort, "max", &efforts),
            ],
        })
        .unwrap();
    send(
        &mut fixture,
        &child.id,
        Update::Usage {
            used: 10,
            size: 100,
        },
    );

    let view = fixture.app.session_view().unwrap();

    assert_eq!(view.id, child.id);
    assert_eq!(view.breadcrumb, [project.name.as_str(), "parent", "child"]);
    assert_eq!(view.status, Status::Running);
    assert!(view.running);
    assert!(!view.auto_allow);
    assert_eq!(view.entries, [Entry::User("hi".into())]);
    assert!(std::ptr::eq(
        view.entries,
        fixture.app.session_view().unwrap().entries
    ));
    assert_eq!(view.revision, 2);
    assert_eq!(view.usage, Some((10, 100)));
    assert_eq!(
        view.model,
        Some(ChoiceView {
            current: ConfigValueId::new("fast"),
            current_name: "Fast".into(),
            choices: vec![choice("fast", "Fast"), choice("opus", "Opus")],
        })
    );
    assert_eq!(
        view.effort,
        Some(ChoiceView {
            current: ConfigValueId::new("max"),
            current_name: "max".into(),
            choices: vec![choice("low", "Low"), choice("high", "High")],
        })
    );

    fixture.app.select_session(parent.id.clone()).unwrap();
    let view = fixture.app.session_view().unwrap();
    assert_eq!(view.breadcrumb, [project.name.as_str(), "parent"]);
    assert_eq!(view.status, Status::Idle);
    assert!(!view.running);
    assert_eq!(view.model, None);
    assert_eq!(view.effort, None);
    assert_eq!(view.usage, None);
    assert_eq!(view.revision, 0);
}

#[test]
fn the_env_view_lists_injected_and_skipped_instruction_files() {
    let dir = project_dir(&["api"]);
    fs::write(dir.path().join("AGENTS.md"), "global rules").unwrap();
    fs::write(dir.path().join("api/AGENTS.md"), [0xff, 0xfe]).unwrap();
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    assert!(fixture.app.env_view().is_none());
    let models = [("fast", "Fast")];
    created_session(
        &mut fixture,
        &project,
        Scope::Repository(RepositoryId::try_from("api").unwrap()),
        vec![option("model", ConfigCategory::Model, "fast", &models)],
    );
    let discovered = discover_project(dir.path()).unwrap();
    let reason = instruction_chain(&discovered, discovered.repositories.first()).skipped[0]
        .reason
        .clone();
    let files = |injected| {
        vec![
            InstructionFileView {
                path: dir.path().join("AGENTS.md"),
                scope: InstructionScope::Global,
                injected,
                skipped_reason: None,
            },
            InstructionFileView {
                path: dir.path().join("api/AGENTS.md"),
                scope: InstructionScope::Repository,
                injected: false,
                skipped_reason: Some(reason.clone()),
            },
        ]
    };

    let before = fixture.app.env_view().unwrap();
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    let after = fixture.app.env_view().unwrap();

    assert_eq!(before.instruction_files, files(false));
    assert_eq!(after.instruction_files, files(true));
    assert_eq!(before.breadcrumb, [discovered.name.as_str(), "New session"]);
    assert_eq!(after.breadcrumb, [discovered.name.as_str(), "go"]);
    assert_eq!(after.backend, "claude-agent-acp");
    assert_eq!(after.cwd, dir.path().join("api"));
    assert_eq!(after.model.as_deref(), Some("Fast"));
    assert_eq!(after.effort, None);
}

#[test]
fn an_update_the_store_rejects_is_not_applied() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let old = stored_session(&project, "old", 1);
    let mut fixture = fixture_with(store_with(&project, std::slice::from_ref(&old)));
    fixture.app.select_session(old.id.clone()).unwrap();
    fixture.store.save_sessions(project.id, &[]).unwrap();

    let result = fixture.app.apply_event(BackendEvent::Update {
        session: old.id,
        update: Update::AgentText("lost".into()),
    });

    assert!(matches!(
        result,
        Err(AppError::Store(StoreError::Corrupt { .. }))
    ));
    let view = fixture.app.session_view().unwrap();
    assert!(view.entries.is_empty());
    assert_eq!(view.revision, 0);
}

#[test]
fn closing_a_project_drops_its_waiting_permissions_but_keeps_the_others() {
    let dir = project_dir(&[]);
    let other_dir = project_dir(&[]);
    let mut fixture = fixture();
    let closed = fixture.app.open_project(dir.path()).unwrap();
    let kept = fixture.app.open_project(other_dir.path()).unwrap();
    let options = [("allow", PermissionKind::AllowOnce)];
    let closed_session = created_session(&mut fixture, &closed, Scope::Project, Vec::new());
    send(&mut fixture, &closed_session, permission(1, &options));
    let kept_session = created_session(&mut fixture, &kept, Scope::Project, Vec::new());
    send(&mut fixture, &kept_session, permission(1, &options));

    fixture.app.close_project(closed).unwrap();
    let calls = fixture.backend.calls().len();
    fixture
        .app
        .answer_permission(closed_session, PermissionId(1), None)
        .unwrap();
    assert_eq!(fixture.backend.calls().len(), calls);
    fixture
        .app
        .answer_permission(kept_session.clone(), PermissionId(1), None)
        .unwrap();

    assert_eq!(
        last_call(&fixture.backend),
        Call::RespondPermission {
            session: kept_session,
            permission: PermissionId(1),
            option: None,
        }
    );
    assert_eq!(status(&fixture.app), Status::Running);
}
