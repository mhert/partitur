#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use tempfile::TempDir;

use super::*;
use crate::archive::Archived;
use crate::backend::{
    ConfigCategory, ConfigChoice, PermissionKind, PermissionOption, PromptContent, StopReason,
    ToolKind,
};
use crate::git::GitAnswer;
use crate::ids::{
    BackendId, BackendInfo, BackendSessionId, ConfigId, ConfigValueId, GitRequestId, PermissionId,
    PermissionOptionId, ToolCallId,
};
use crate::instructions::{InstructionScope, inject, instruction_chain};
use crate::session::{BranchChoice, BranchMode};
use crate::session::{Fold, title_from_prompt};
use crate::settings::{Level, ProjectSettings, RepositorySettings, SettingsLayer};
use crate::testing::{Call, MemoryGit, MemoryStore, RecordingBackend, settle, single_claude};
use crate::transcript::Entry;
use crate::view_model::SessionSummary;
use crate::view_model::{ChoiceView, InstructionFileView, SessionNode};

struct Fixture {
    app: App,
    backend: RecordingBackend,
    store: MemoryStore,
    git: MemoryGit,
}

fn fixture() -> Fixture {
    fixture_with(MemoryStore::new())
}

fn fixture_with(store: MemoryStore) -> Fixture {
    let (backends, default) = single_claude();
    fixture_on(store, backends, default)
}

/// A fixture whose app knows `backends` and gives new sessions `default`.
fn fixture_on(store: MemoryStore, backends: Vec<BackendInfo>, default: BackendId) -> Fixture {
    let backend = RecordingBackend::new();
    let git = MemoryGit::new();
    let app = App::load(
        Box::new(backend.clone()),
        Box::new(store.clone()),
        Box::new(git.clone()),
        backends,
        default,
        GlobalSettings::default(),
    )
    .unwrap();
    Fixture {
        app,
        backend,
        store,
        git,
    }
}

fn backend_id(name: &str) -> BackendId {
    BackendId::try_from(name).unwrap()
}

/// Two configured backends, `a` and `b`.
fn backends_a_and_b() -> Vec<BackendInfo> {
    ["a", "b"]
        .into_iter()
        .map(|name| BackendInfo {
            id: backend_id(name),
            name: name.into(),
            command: format!("agent-{name}"),
        })
        .collect()
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
        backend: backend_id("claude"),
        model: None,
        effort: None,
        status: Status::Idle,
        parent: None,
        auto_allow: false,
        instructions_injected: false,
        created_at,
        orchestrator: false,
        archived: None,
        workspace: Workspace::CheckedOut,
        child_defaults: None,
        first_prompt: String::new(),
        skill: None,
        title_source: crate::TitleSource::Derived,
        last_activity: 0,
        children: crate::Fold::Folded,
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
fn load_marks_sessions_left_in_a_turn_as_interrupted() {
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
    let idle = stored_session(&project, "idle", 5);
    let mut archived = stored_session(&project, "archived", 6);
    archived.status = Status::Running;
    archived.archived = Some(Archived { at: 10 });
    let store = MemoryStore::new();
    store.save_projects(std::slice::from_ref(&project)).unwrap();
    store
        .save_sessions(
            project.id.clone(),
            &[
                running.clone(),
                waiting.clone(),
                done.clone(),
                failed.clone(),
                idle.clone(),
                archived.clone(),
            ],
        )
        .unwrap();
    let asked = permission(1, &[("allow", PermissionKind::AllowOnce)]);
    store.append_update(waiting.id.clone(), &asked).unwrap();

    let fixture = fixture_with(store.clone());

    let tree = fixture.app.tree();
    assert_eq!(tree.projects.len(), 1);
    assert_eq!(tree.projects[0].id, project.id);
    let status_of = |id: &SessionId| session_status(&fixture.app, id);
    assert_eq!(status_of(&running.id), Status::Interrupted);
    assert_eq!(status_of(&waiting.id), Status::Interrupted);
    assert_eq!(status_of(&archived.id), Status::Interrupted);
    assert_eq!(status_of(&done.id), Status::Idle);
    assert_eq!(status_of(&failed.id), Status::Idle);
    assert_eq!(status_of(&idle.id), Status::Idle);
    let notice = Update::Notice(INTERRUPTED.into());
    for noted in [&running, &archived] {
        assert_eq!(
            store.load_updates(noted.id.clone()).unwrap(),
            std::slice::from_ref(&notice)
        );
    }
    assert_eq!(
        store.load_updates(waiting.id.clone()).unwrap(),
        [asked, notice]
    );
    for quiet in [&done, &failed, &idle] {
        assert!(store.load_updates(quiet.id.clone()).unwrap().is_empty());
    }
    let restored = fixture
        .app
        .sessions
        .iter()
        .find(|s| s.id == archived.id)
        .unwrap();
    assert_eq!(restored.archived, Some(Archived { at: 10 }));
    let saved = store.load_sessions(project.id.clone()).unwrap();
    assert_eq!(
        saved.iter().find(|s| s.id == running.id).unwrap().status,
        Status::Interrupted
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
fn an_interrupted_session_is_noted_once_across_restarts() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let mut running = stored_session(&project, "running", 1);
    running.status = Status::Running;
    let store = MemoryStore::new();
    store.save_projects(std::slice::from_ref(&project)).unwrap();
    store
        .save_sessions(project.id.clone(), std::slice::from_ref(&running))
        .unwrap();

    drop(fixture_with(store.clone()));
    let again = fixture_with(store.clone());

    assert_eq!(
        store.load_updates(running.id.clone()).unwrap(),
        [Update::Notice(INTERRUPTED.into())]
    );
    assert_eq!(session_status(&again.app, &running.id), Status::Interrupted);
}

#[test]
fn a_load_that_finds_nothing_left_in_a_turn_saves_nothing() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let mut done = stored_session(&project, "done", 1);
    done.status = Status::Done;
    let store = MemoryStore::new();
    store.save_projects(std::slice::from_ref(&project)).unwrap();
    store
        .save_sessions(project.id.clone(), std::slice::from_ref(&done))
        .unwrap();

    drop(fixture_with(store.clone()));

    assert_eq!(
        store.load_sessions(project.id.clone()).unwrap()[0].status,
        Status::Done
    );
}

#[test]
fn prompting_an_interrupted_session_resumes_it() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();

    let mut restarted = fixture_with(fixture.store.clone());
    assert_eq!(
        session_status(&restarted.app, &session),
        Status::Interrupted
    );
    restarted.app.select_session(session.clone()).unwrap();
    assert!(entries(&restarted.app).contains(&Entry::Notice(INTERRUPTED.into())));

    restarted.app.submit_prompt("go on", Vec::new()).unwrap();

    assert_eq!(session_status(&restarted.app, &session), Status::Running);
    load_request(&restarted.backend);
}

#[test]
fn load_rejects_an_empty_backend_list_and_an_unknown_default() {
    let load = |backends: Vec<BackendInfo>, default: BackendId| {
        App::load(
            Box::new(RecordingBackend::new()),
            Box::new(MemoryStore::new()),
            Box::new(MemoryGit::new()),
            backends,
            default,
            GlobalSettings::default(),
        )
    };

    let empty = load(Vec::new(), backend_id("a"));
    let unknown = load(backends_a_and_b(), backend_id("c"));
    let known = load(backends_a_and_b(), backend_id("b")).unwrap();

    assert!(matches!(empty, Err(AppError::NoBackends)));
    assert!(matches!(unknown, Err(AppError::NoBackends)));
    assert_eq!(known.backends(), backends_a_and_b().as_slice());
    assert_eq!(known.default_backend(), &backend_id("b"));
    assert_eq!(
        AppError::NoBackends.to_string(),
        "no configured backend matches the default backend"
    );
    assert_eq!(
        AppError::UnknownBackend(backend_id("c")).to_string(),
        "backend c is no longer configured; pick another backend for this session"
    );
}

#[test]
fn sessions_without_a_backend_get_the_default_on_load_and_are_saved_once() {
    let dir = project_dir(&[]);
    let other_dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let other = discover_project(other_dir.path()).unwrap();
    let mut json = serde_json::to_value(stored_session(&project, "old", 1)).unwrap();
    json.as_object_mut().unwrap().remove("backend");
    let old: Session = serde_json::from_value(json).unwrap();
    assert!(old.backend.is_unset());
    let mut on_b = stored_session(&project, "on b", 2);
    on_b.backend = backend_id("b");
    let mut untouched = stored_session(&other, "untouched", 3);
    untouched.backend = backend_id("b");
    let store = MemoryStore::new();
    store
        .save_projects(&[project.clone(), other.clone()])
        .unwrap();
    store
        .save_sessions(project.id.clone(), &[old.clone(), on_b.clone()])
        .unwrap();
    store
        .save_sessions(other.id.clone(), std::slice::from_ref(&untouched))
        .unwrap();

    let fixture = fixture_on(store.clone(), backends_a_and_b(), backend_id("a"));

    let backend_of = |id: &SessionId| {
        fixture
            .app
            .sessions
            .iter()
            .find(|session| &session.id == id)
            .unwrap()
            .backend
            .clone()
    };
    assert_eq!(backend_of(&old.id), backend_id("a"));
    assert_eq!(backend_of(&on_b.id), backend_id("b"));
    assert_eq!(backend_of(&untouched.id), backend_id("b"));
    assert_eq!(saved(&store, &project.id, &old.id).backend, backend_id("a"));
    assert_eq!(
        saved(&store, &project.id, &on_b.id).backend,
        backend_id("b")
    );
    // One save when seeding, one for the migration; the other project only its seed.
    assert_eq!(store.session_saves(&project.id), 2);
    assert_eq!(store.session_saves(&other.id), 1);
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
    assert_eq!(session.title, "untitled");
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
fn new_sessions_run_on_the_default_backend() {
    let dir = project_dir(&[]);
    let mut fixture = fixture_on(MemoryStore::new(), backends_a_and_b(), backend_id("b"));
    let project = fixture.app.open_project(dir.path()).unwrap();

    let session = fixture
        .app
        .new_session(project.clone(), Scope::Project)
        .unwrap();

    let (request, _, _) = new_session_request(&fixture.backend);
    assert_eq!(
        fixture.backend.calls(),
        [Call::NewSession {
            request,
            session: session.clone(),
            backend: backend_id("b"),
            cwd: fs::canonicalize(dir.path()).unwrap(),
            orchestrator: false,
        }]
    );
    assert_eq!(
        saved(&fixture.store, &project, &session).backend,
        backend_id("b")
    );
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
        BackendEvent::SessionClosed {
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
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());

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
fn an_agent_title_names_an_untitled_session() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    let saves = fixture.store.session_saves(&project);

    send(
        &mut fixture,
        &session,
        Update::Title("Worktree support".into()),
    );

    assert_eq!(
        fixture.app.session(&session).unwrap().title,
        "Worktree support"
    );
    assert_eq!(
        fixture.app.session(&session).unwrap().title_source,
        crate::TitleSource::Derived
    );
    let stored = fixture.store.load_sessions(project.clone()).unwrap();
    assert_eq!(stored[0].title, "Worktree support");
    assert_eq!(fixture.store.session_saves(&project), saves + 1);
    assert!(fixture.store.load_updates(session).unwrap().is_empty());
}

#[test]
fn an_agent_title_never_replaces_a_given_title() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture
        .app
        .change_session(session.clone(), |session| {
            "Mine".clone_into(&mut session.title);
            session.title_source = crate::TitleSource::Given;
        })
        .unwrap();
    let saves = fixture.store.session_saves(&project);

    send(
        &mut fixture,
        &session,
        Update::Title("Worktree support".into()),
    );

    assert_eq!(fixture.app.session(&session).unwrap().title, "Mine");
    assert_eq!(fixture.store.session_saves(&project), saves);
    assert!(fixture.store.load_updates(session).unwrap().is_empty());
}

#[test]
fn an_agent_title_for_a_deleted_session_is_ignored() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.delete(session.clone(), &[]).unwrap();

    send(
        &mut fixture,
        &session,
        Update::Title("Worktree support".into()),
    );

    assert!(fixture.store.load_updates(session).unwrap().is_empty());
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
    // Its connection is open although its transcript was never shown.
    fixture.app.attached.insert(stored.id.clone());

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
fn a_permission_left_unanswered_by_a_quit_is_cancelled_when_its_transcript_loads() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    send(
        &mut fixture,
        &session,
        permission(1, &[("allow", PermissionKind::AllowOnce)]),
    );
    assert_eq!(
        session_status(&fixture.app, &session),
        Status::WaitingForYou
    );

    let mut restarted = fixture_with(fixture.store.clone());
    restarted.app.select_session(session.clone()).unwrap();

    let entries = entries(&restarted.app);
    assert_eq!(entries.len(), 3);
    assert_eq!(answer_of(&entries[1]), None);
    assert_eq!(entries[2], Entry::Notice(INTERRUPTED.into()));
    assert_eq!(
        answered_updates(&restarted.store, &session),
        [Update::PermissionAnswered {
            id: PermissionId(1),
            option: None,
        }]
    );
    assert!(!restarted.app.session_view().unwrap().running());
}

#[test]
fn a_permission_a_session_still_waits_on_stays_open_when_its_transcript_loads() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let stored = stored_session(&project, "old", 1);
    let store = MemoryStore::new();
    store.save_projects(std::slice::from_ref(&project)).unwrap();
    store
        .save_sessions(project.id, std::slice::from_ref(&stored))
        .unwrap();
    let mut fixture = fixture_with(store);
    // Its connection is open although its transcript was never shown.
    fixture.app.attached.insert(stored.id.clone());
    send(
        &mut fixture,
        &stored.id,
        permission(1, &[("allow", PermissionKind::AllowOnce)]),
    );
    assert!(!fixture.app.transcripts.contains_key(&stored.id));

    fixture.app.select_session(stored.id.clone()).unwrap();

    assert!(matches!(
        entries(&fixture.app).as_slice(),
        [Entry::Permission { answer: None, .. }]
    ));
    assert_eq!(answered_updates(&fixture.store, &stored.id), []);
    assert!(fixture.app.session_view().unwrap().running());
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
    assert_eq!(first.sessions[0].scope_label, "");
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
fn closing_a_project_hides_it_but_keeps_the_others() {
    let dir = project_dir(&[]);
    let other_dir = project_dir(&[]);
    let mut fixture = fixture();
    let closed = fixture.app.open_project(dir.path()).unwrap();
    let kept = fixture.app.open_project(other_dir.path()).unwrap();
    let closed_session = fixture
        .app
        .new_session(closed.clone(), Scope::Project)
        .unwrap();
    fixture
        .app
        .new_session(kept.clone(), Scope::Project)
        .unwrap();
    fixture.app.select_session(closed_session).unwrap();

    fixture.app.close_project(closed.clone()).unwrap();

    let tree = fixture.app.tree();
    assert_eq!(tree.projects.len(), 1);
    assert_eq!(tree.projects[0].id, kept);
    assert_eq!(tree.projects[0].sessions.len(), 1);
    assert_eq!(fixture.app.current_session(), None);
    let saved: Vec<_> = fixture
        .store
        .load_projects()
        .unwrap()
        .into_iter()
        .map(|project| (project.id, project.closed))
        .collect();
    assert_eq!(saved, [(closed.clone(), true), (kept.clone(), false)]);
    assert_eq!(
        fixture.store.load_sessions(closed.clone()).unwrap().len(),
        1
    );
    assert_eq!(fixture.store.load_sessions(kept).unwrap().len(), 1);
    assert!(matches!(
        fixture.app.close_project(closed.clone()),
        Err(AppError::UnknownProject(id)) if id == closed
    ));
}

#[test]
fn closing_a_project_keeps_its_sessions_and_reopening_restores_them() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    send(
        &mut fixture,
        &session,
        Update::TurnEnded(StopReason::EndTurn),
    );
    fixture.app.close_project(project.clone()).unwrap();
    fs::create_dir_all(dir.path().join("api/.git")).unwrap();

    let reopened = fixture.app.open_project(&dir.path().join(".")).unwrap();

    assert_eq!(reopened, project);
    let tree = fixture.app.tree();
    assert_eq!(tree.projects.len(), 1);
    assert_eq!(tree.projects[0].repositories.len(), 1);
    assert_eq!(titles(&tree.projects[0].sessions), ["untitled"]);
    assert!(!fixture.store.load_projects().unwrap()[0].closed);

    fixture.app.close_project(project.clone()).unwrap();
    let mut restarted = fixture_with(fixture.store.clone());
    assert!(restarted.app.tree().projects.is_empty());

    let reopened = restarted.app.open_project(dir.path()).unwrap();

    assert_eq!(reopened, project);
    let tree = restarted.app.tree();
    assert_eq!(titles(&tree.projects[0].sessions), ["untitled"]);
    restarted.app.select_session(session).unwrap();
    assert_eq!(
        entries(&restarted.app),
        [
            Entry::User("go".into()),
            Entry::TurnEnded(StopReason::EndTurn)
        ]
    );
}

#[test]
fn closing_a_project_cancels_its_running_sessions() {
    let dir = project_dir(&[]);
    let other_dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let other = fixture.app.open_project(other_dir.path()).unwrap();
    let options = [("allow", PermissionKind::AllowOnce)];
    let running = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("run", Vec::new()).unwrap();
    let waiting = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("wait", Vec::new()).unwrap();
    send(&mut fixture, &waiting, permission(1, &options));
    created_session(&mut fixture, &project, Scope::Project, Vec::new());
    created_session(&mut fixture, &other, Scope::Project, Vec::new());
    fixture.app.submit_prompt("elsewhere", Vec::new()).unwrap();
    let cancel = |session: &SessionId| Call::Cancel {
        session: session.clone(),
    };
    let before = fixture.backend.calls().len();

    fixture.backend.fail_next_call();
    let unavailable = fixture.app.close_project(project.clone());

    assert!(matches!(
        unavailable,
        Err(AppError::Backend(BackendError::Unavailable(_)))
    ));
    assert_eq!(fixture.app.tree().projects.len(), 2);
    assert_eq!(fixture.backend.calls()[before..], [cancel(&running)]);

    let before = fixture.backend.calls().len();
    fixture
        .backend
        .fail_next_call_with(BackendError::UnknownSession(running.clone()));
    fixture.app.close_project(project).unwrap();

    assert_eq!(
        fixture.backend.calls()[before..],
        [
            cancel(&running),
            Call::RespondPermission {
                session: waiting.clone(),
                permission: PermissionId(1),
                option: None,
            },
            cancel(&waiting),
        ]
    );
    assert_eq!(fixture.app.tree().projects.len(), 1);
    send(
        &mut fixture,
        &running,
        Update::TurnEnded(StopReason::Cancelled),
    );
    assert_eq!(session_status(&fixture.app, &running), Status::Done);
}

#[test]
fn a_relative_root_is_stored_canonical() {
    let dir = project_dir(&["api"]);
    let canonical = fs::canonicalize(dir.path()).unwrap();
    let mut fixture = fixture();

    let project = fixture
        .app
        .open_project(&dir.path().join("api").join(".."))
        .unwrap();

    let saved = fixture.store.load_projects().unwrap();
    assert_eq!(saved[0].id, project);
    assert_eq!(saved[0].root, canonical);
    assert_eq!(saved[0].repositories[0].path, canonical.join("api"));
    assert_eq!(
        fixture.app.open_project(&canonical.join(".")).unwrap(),
        project
    );
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
pub(super) fn load_request(backend: &RecordingBackend) -> RequestId {
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

/// The options an agent reports while it runs `model`, as claude-agent-acp builds them: the
/// Effort option lists only the efforts the current model supports, on `high`, and is
/// missing for `haiku`, which has none. `sonnet` lacks the `xhigh` that `opus` offers.
pub(super) fn agent_options(model: &str) -> Vec<ConfigOption> {
    let models = [("sonnet", "Sonnet"), ("opus", "Opus"), ("haiku", "Haiku")];
    let efforts: &[(&str, &str)] = match model {
        "opus" => &[("low", "Low"), ("high", "High"), ("xhigh", "Extra high")],
        "haiku" => &[],
        _ => &[("low", "Low"), ("high", "High")],
    };
    let mut options = vec![option("model", ConfigCategory::Model, model, &models)];
    if !efforts.is_empty() {
        options.push(option("effort", ConfigCategory::Effort, "high", efforts));
    }
    options
}

fn permission(id: u64, options: &[(&str, PermissionKind)]) -> Update {
    permission_for(id, "Edit main.rs", ToolKind::Edit, None, options)
}

/// A permission request for the call `title` of `kind`, from the tool `name`.
fn permission_for(
    id: u64,
    title: &str,
    kind: ToolKind,
    name: Option<&str>,
    options: &[(&str, PermissionKind)],
) -> Update {
    Update::PermissionRequested {
        id: PermissionId(id),
        tool_call_id: ToolCallId::new("t-1"),
        title: title.into(),
        kind,
        options: options
            .iter()
            .map(|(id, kind)| PermissionOption {
                id: PermissionOptionId::new(*id),
                name: (*id).into(),
                kind: *kind,
            })
            .collect(),
        name: name.map(str::to_owned),
        locations: Vec::new(),
        partitur_tool: false,
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
fn the_first_prompt_injects_instructions_and_keeps_the_title() {
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
    assert_eq!(view.breadcrumb.last().unwrap(), "untitled");
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
    assert_eq!(saved.title, "untitled");
    assert_eq!(saved.status, Status::Running);
}

#[test]
fn the_first_prompt_records_prompt_and_skill_and_leaves_the_title() {
    let dir = project_dir(&["api"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());

    fixture
        .app
        .submit_prompt("/brainstorming Worktree support", Vec::new())
        .unwrap();
    let finished = fixture.app.session(&session).unwrap().clone();
    send(
        &mut fixture,
        &session,
        Update::TurnEnded(StopReason::EndTurn),
    );
    fixture.app.submit_prompt("and more", Vec::new()).unwrap();

    let after = saved(&fixture.store, &project, &session);
    assert_eq!(finished.first_prompt, "/brainstorming Worktree support");
    assert_eq!(finished.skill.as_deref(), Some("/brainstorming"));
    assert_eq!(finished.title, "untitled");
    assert_eq!(after.first_prompt, "/brainstorming Worktree support");
    assert_eq!(after.skill.as_deref(), Some("/brainstorming"));
    assert_eq!(after.title, "untitled");
}

#[test]
fn a_session_with_history_never_takes_a_later_prompt_as_its_first() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let old = stored_session(&project, "old", 1);
    let store = store_with(&project, std::slice::from_ref(&old));
    store
        .append_update(old.id.clone(), &Update::UserText("before".into()))
        .unwrap();
    let mut fixture = fixture_with(store);
    fixture.app.select_session(old.id.clone()).unwrap();

    fixture
        .app
        .submit_prompt("/brainstorming later", Vec::new())
        .unwrap();

    let after = saved(&fixture.store, &project.id, &old.id);
    assert_eq!(after.first_prompt, "");
    assert_eq!(after.skill, None);
}

#[test]
fn an_attachment_only_first_prompt_leaves_the_first_prompt_empty() {
    let dir = project_dir(&[]);
    let files = TempDir::new().unwrap();
    fs::write(files.path().join("a.png"), "hi").unwrap();
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());

    let image = Attachment {
        path: files.path().join("a.png"),
    };
    fixture.app.submit_prompt("", vec![image]).unwrap();
    send(
        &mut fixture,
        &session,
        Update::TurnEnded(StopReason::EndTurn),
    );
    fixture
        .app
        .submit_prompt("/brainstorming later", Vec::new())
        .unwrap();

    let after = saved(&fixture.store, &project, &session);
    assert_eq!(after.first_prompt, "");
    assert_eq!(after.skill, None);
}

#[test]
fn a_notice_before_the_first_prompt_does_not_make_it_a_later_one() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    send(&mut fixture, &session, Update::Notice("heads up".into()));

    fixture
        .app
        .submit_prompt("/brainstorming first", Vec::new())
        .unwrap();

    let after = saved(&fixture.store, &project, &session);
    assert_eq!(after.first_prompt, "/brainstorming first");
    assert_eq!(after.skill.as_deref(), Some("/brainstorming"));
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
    assert_eq!(view.breadcrumb.last().unwrap(), "untitled");
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
fn a_restored_session_is_loaded_on_its_own_backend() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let mut old = stored_session(&project, "old", 1);
    old.backend = backend_id("b");
    let mut fixture = fixture_on(
        store_with(&project, std::slice::from_ref(&old)),
        backends_a_and_b(),
        backend_id("a"),
    );
    fixture.app.select_session(old.id.clone()).unwrap();

    fixture.app.submit_prompt("again", Vec::new()).unwrap();

    let request = load_request(&fixture.backend);
    assert_eq!(
        last_call(&fixture.backend),
        Call::LoadSession {
            request,
            session: old.id.clone(),
            backend: backend_id("b"),
            backend_session: old.backend_session.clone().unwrap(),
            cwd: project.root.clone(),
            orchestrator: false,
        }
    );

    // A stored session that no longer loads is replaced on the same backend.
    fixture
        .app
        .apply_event(BackendEvent::RequestFailed {
            request,
            session: old.id.clone(),
            error: "gone".into(),
        })
        .unwrap();

    let (fresh, _, _) = new_session_request(&fixture.backend);
    assert_eq!(
        last_call(&fixture.backend),
        Call::NewSession {
            request: fresh,
            session: old.id.clone(),
            backend: backend_id("b"),
            cwd: project.root.clone(),
            orchestrator: false,
        }
    );
}

#[test]
fn a_load_the_backend_cannot_queue_leaves_the_stored_session_unchanged() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let old = stored_session(&project, "old", 1);
    let mut fixture = fixture_with(store_with(&project, std::slice::from_ref(&old)));
    fixture.app.select_session(old.id.clone()).unwrap();
    fixture.backend.fail_next_call();

    let result = fixture.app.submit_prompt("again", Vec::new());

    assert!(matches!(
        result,
        Err(AppError::Backend(BackendError::Unavailable(_)))
    ));
    assert!(matches!(
        last_call(&fixture.backend),
        Call::LoadSession { .. }
    ));
    assert!(fixture.app.pending.is_empty());
    assert!(fixture.app.queued.is_empty());
    assert_eq!(status(&fixture.app), Status::Idle);
    assert!(entries(&fixture.app).is_empty());
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
            backend: old.backend.clone(),
            backend_session: old.backend_session.clone().unwrap(),
            cwd: project.root.clone(),
            orchestrator: false,
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

    send(
        &mut fixture,
        &old.id,
        Update::TurnEnded(StopReason::EndTurn),
    );
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
    assert_eq!(view.breadcrumb.last().unwrap(), "untitled");
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
    assert_eq!(view.breadcrumb.last().unwrap(), "untitled");
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
    assert_eq!(view.breadcrumb.last().unwrap(), "untitled");
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
fn the_session_view_lists_the_repositories_and_the_orchestrator_flag() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    created_session(&mut fixture, &project, Scope::Project, Vec::new());

    let view = fixture.app.session_view().unwrap();

    let names: Vec<_> = view.repositories.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["api", "web"]);
    assert!(view.orchestrator);
    assert!(!view.attention);
}

#[test]
fn the_session_view_leaves_out_a_root_repository_and_flags_no_orchestrator() {
    let dir = project_dir(&[]);
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    created_session(&mut fixture, &project, Scope::Project, Vec::new());

    let view = fixture.app.session_view().unwrap();

    assert!(view.repositories.is_empty());
    assert!(!view.orchestrator);
}

#[test]
fn the_session_view_shows_an_orchestrators_child_defaults() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let orchestrator = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    assert_eq!(fixture.app.session_view().unwrap().child_defaults, None);
    let defaults = BranchChoice {
        mode: BranchMode::Worktree,
        branch: "feat/x".into(),
        base: Some("main".into()),
        fetch: false,
    };
    fixture
        .app
        .sessions
        .iter_mut()
        .find(|session| session.id == orchestrator)
        .unwrap()
        .child_defaults = Some(defaults.clone());

    let view = fixture.app.session_view().unwrap();

    assert_eq!(view.child_defaults, Some(defaults));
}

#[test]
fn an_answer_nothing_waits_for_is_ignored() {
    let dir = project_dir(&["api"]);
    let mut fixture = fixture();
    fixture.app.open_project(dir.path()).unwrap();
    let before = fixture.app.tree();

    let answered = fixture.app.apply_git_event(GitEvent::Answered {
        request: GitRequestId(99),
        answers: vec![Ok(GitAnswer::Changes(0))],
    });
    let ran = fixture.app.apply_git_event(GitEvent::Ran {
        request: GitRequestId(100),
        done: 0,
        result: Ok(()),
    });

    assert!(answered.is_ok());
    assert!(ran.is_ok());
    assert_eq!(fixture.app.tree(), before);
}

#[test]
fn settling_feeds_every_waiting_answer_to_the_app() {
    let mut fixture = fixture();
    fixture.git.query(GitRequestId(1), Vec::new()).unwrap();
    fixture.git.run(GitRequestId(2), Vec::new()).unwrap();

    let errors = settle(&mut fixture.app, &fixture.git);

    assert!(errors.is_empty());
    assert_eq!(fixture.git.waiting(), 0);
}

#[test]
fn the_session_view_knows_when_a_child_waits() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let parent = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    let api = RepositoryId::try_from("api").unwrap();
    let child = created_session(&mut fixture, &project, Scope::Repository(api), Vec::new());
    fixture
        .app
        .sessions
        .iter_mut()
        .find(|session| session.id == child)
        .unwrap()
        .parent = Some(parent.clone());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    send(
        &mut fixture,
        &child,
        permission(1, &[("allow", PermissionKind::AllowOnce)]),
    );
    fixture.app.select_session(parent).unwrap();

    let view = fixture.app.session_view().unwrap();

    assert!(view.attention);
    assert_ne!(view.status, Status::WaitingForYou);
    assert!(view.repositories.iter().any(|r| r.name == "web"));
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
    let lapsed = |id| Update::PermissionAnswered {
        id: PermissionId(id),
        option: None,
    };
    assert_eq!(
        answered_updates(&fixture.store, &session),
        [
            lapsed(1),
            lapsed(2),
            Update::PermissionAnswered {
                id: PermissionId(3),
                option: allow()
            }
        ]
    );
}

#[test]
fn a_permission_still_waiting_when_the_turn_ends_is_closed() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    let options = [("allow", PermissionKind::AllowOnce)];
    let lapsed = |id| Update::PermissionAnswered {
        id: PermissionId(id),
        option: None,
    };
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    send(&mut fixture, &session, permission(1, &options));
    send(&mut fixture, &session, permission(2, &options));
    let calls = fixture.backend.calls().len();

    send(
        &mut fixture,
        &session,
        Update::TurnEnded(StopReason::Cancelled),
    );

    assert_eq!(fixture.backend.calls().len(), calls);
    assert_eq!(status(&fixture.app), Status::Done);
    let entries = entries(&fixture.app);
    assert_eq!(answer_of(&entries[1]), None);
    assert_eq!(answer_of(&entries[2]), None);
    assert_eq!(
        answered_updates(&fixture.store, &session),
        [lapsed(1), lapsed(2)]
    );

    fixture.app.submit_prompt("again", Vec::new()).unwrap();
    send(&mut fixture, &session, permission(3, &options));
    send(&mut fixture, &session, Update::Failed("crashed".into()));

    assert_eq!(fixture.backend.calls().len(), calls + 1);
    assert_eq!(status(&fixture.app), Status::Failed);
    assert_eq!(
        answered_updates(&fixture.store, &session),
        [lapsed(1), lapsed(2), lapsed(3)]
    );
    let reloaded = Transcript::from_updates(&fixture.store.load_updates(session).unwrap());
    assert!(
        reloaded
            .entries()
            .iter()
            .all(|entry| !matches!(entry, Entry::Permission { answer: None, .. }))
    );
}

#[test]
fn a_prompt_while_a_turn_runs_is_refused() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    let options = [("allow", PermissionKind::AllowOnce)];
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    let calls = fixture.backend.calls();
    let updates = fixture.store.load_updates(session.clone()).unwrap();
    let before = entries(&fixture.app);

    let running = fixture.app.submit_prompt("more", Vec::new());

    assert!(matches!(running, Err(AppError::TurnInProgress)));
    assert_eq!(fixture.backend.calls(), calls);
    assert_eq!(entries(&fixture.app), before);
    assert_eq!(
        fixture.store.load_updates(session.clone()).unwrap(),
        updates
    );

    send(&mut fixture, &session, permission(1, &options));
    let calls = fixture.backend.calls();
    let updates = fixture.store.load_updates(session.clone()).unwrap();
    let before = entries(&fixture.app);

    let waiting = fixture.app.submit_prompt("more", Vec::new());

    assert!(matches!(waiting, Err(AppError::TurnInProgress)));
    assert_eq!(status(&fixture.app), Status::WaitingForYou);
    assert!(fixture.app.session_view().unwrap().running());
    assert_eq!(fixture.backend.calls(), calls);
    assert_eq!(entries(&fixture.app), before);
    assert_eq!(fixture.store.load_updates(session).unwrap(), updates);
    assert_eq!(
        AppError::TurnInProgress.to_string(),
        "a turn is still running; stop it or wait for it to end"
    );
}

#[test]
fn turn_ended_sets_done_and_failed_sets_failed() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    assert!(fixture.app.session_view().unwrap().running());

    send(
        &mut fixture,
        &session,
        Update::TurnEnded(StopReason::EndTurn),
    );
    assert_eq!(status(&fixture.app), Status::Done);
    assert!(!fixture.app.session_view().unwrap().running());
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
    assert!(view.running());
    assert!(!view.auto_allow);
    assert_eq!(view.entries, [Entry::User("hi".into())]);
    assert!(std::ptr::eq(
        view.entries,
        fixture.app.session_view().unwrap().entries
    ));
    assert_eq!(view.revision, 2);
    assert_eq!(view.usage, Some((10, 100)));
    // The tree knows the usage of the session whose transcript is loaded, and of no other.
    let tree = fixture.app.tree();
    let parent_node = &tree.projects[0].sessions[0];
    assert_eq!(parent_node.usage, None);
    assert_eq!(parent_node.children[0].usage, Some((10, 100)));
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
    assert!(!view.running());
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
    assert_eq!(before.breadcrumb, [discovered.name.as_str(), "untitled"]);
    assert_eq!(after.breadcrumb, [discovered.name.as_str(), "untitled"]);
    assert_eq!(after.backend, "claude");
    assert_eq!(after.command.as_deref(), Some("claude-agent-acp"));
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
    fixture.app.attached.insert(old.id.clone());
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

#[test]
fn cancel_answers_waiting_permissions_before_cancelling_the_turn() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    let options = [("allow", PermissionKind::AllowOnce)];
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    send(&mut fixture, &session, permission(2, &options));
    send(&mut fixture, &session, permission(1, &options));
    let before = fixture.backend.calls().len();

    fixture.app.cancel().unwrap();

    let cancelled = |id| Call::RespondPermission {
        session: session.clone(),
        permission: PermissionId(id),
        option: None,
    };
    assert_eq!(
        fixture.backend.calls()[before..],
        [
            cancelled(1),
            cancelled(2),
            Call::Cancel {
                session: session.clone()
            }
        ]
    );
    let entries = entries(&fixture.app);
    assert_eq!(answer_of(&entries[1]), None);
    assert_eq!(answer_of(&entries[2]), None);
    assert_eq!(status(&fixture.app), Status::Running);
    send(
        &mut fixture,
        &session,
        Update::TurnEnded(StopReason::Cancelled),
    );
    assert_eq!(status(&fixture.app), Status::Done);
}

#[test]
fn a_restored_model_is_reapplied_after_the_load_when_the_backend_differs() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let mut old = stored_session(&project, "old", 1);
    old.model = Some(ConfigValueId::new("opus"));
    old.effort = Some(ConfigValueId::new("high"));
    let mut fixture = fixture_with(store_with(&project, std::slice::from_ref(&old)));
    fixture.app.select_session(old.id.clone()).unwrap();
    fixture.app.submit_prompt("again", Vec::new()).unwrap();
    let models = [("sonnet", "Sonnet"), ("opus", "Opus")];
    let efforts = [("low", "Low"), ("high", "High")];

    fixture
        .app
        .apply_event(BackendEvent::SessionLoaded {
            request: load_request(&fixture.backend),
            session: old.id.clone(),
            config: vec![
                option("model", ConfigCategory::Model, "sonnet", &models),
                option("effort", ConfigCategory::Effort, "high", &efforts),
            ],
        })
        .unwrap();
    send(
        &mut fixture,
        &old.id,
        Update::ConfigOptions(vec![
            option("model", ConfigCategory::Model, "opus", &models),
            option("effort", ConfigCategory::Effort, "high", &efforts),
        ]),
    );

    assert_eq!(
        fixture.backend.calls()[1..],
        [
            Call::SetConfig {
                session: old.id.clone(),
                option: ConfigId::new("model"),
                value: ConfigValueId::new("opus"),
            },
            prompt_call(&old.id, "again")
        ]
    );
    let view = fixture.app.session_view().unwrap();
    assert_eq!(view.model.unwrap().current, ConfigValueId::new("opus"));
    assert_eq!(view.effort.unwrap().current, ConfigValueId::new("high"));
}

#[test]
fn a_restored_model_the_backend_does_not_offer_is_left_alone() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let mut old = stored_session(&project, "old", 1);
    old.model = Some(ConfigValueId::new("opus"));
    let mut fixture = fixture_with(store_with(&project, std::slice::from_ref(&old)));
    fixture.app.select_session(old.id.clone()).unwrap();
    fixture.app.submit_prompt("again", Vec::new()).unwrap();

    fixture
        .app
        .apply_event(BackendEvent::SessionLoaded {
            request: load_request(&fixture.backend),
            session: old.id.clone(),
            config: vec![option("effort", ConfigCategory::Effort, "low", &[])],
        })
        .unwrap();

    assert_eq!(
        fixture.backend.calls()[1..],
        [prompt_call(&old.id, "again")]
    );
    assert_eq!(
        saved(&fixture.store, &project.id, &old.id).model,
        Some(ConfigValueId::new("opus"))
    );
}

/// A stored session recording `model` and `effort`, prompted with `again` and loaded by an
/// agent that starts on `sonnet`.
fn resumed_on_sonnet(model: &str, effort: &str) -> (TempDir, Fixture, ProjectId, SessionId) {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let mut old = stored_session(&project, "old", 1);
    old.model = Some(ConfigValueId::new(model));
    old.effort = Some(ConfigValueId::new(effort));
    let mut fixture = fixture_with(store_with(&project, std::slice::from_ref(&old)));
    fixture.app.select_session(old.id.clone()).unwrap();
    fixture.app.submit_prompt("again", Vec::new()).unwrap();
    fixture
        .app
        .apply_event(BackendEvent::SessionLoaded {
            request: load_request(&fixture.backend),
            session: old.id.clone(),
            config: agent_options("sonnet"),
        })
        .unwrap();
    (dir, fixture, project.id, old.id)
}

fn set_call(session: &SessionId, option: &str, value: &str) -> Call {
    Call::SetConfig {
        session: session.clone(),
        option: ConfigId::new(option),
        value: ConfigValueId::new(value),
    }
}

#[test]
fn a_resumed_effort_waits_for_the_options_of_the_model_it_switches_to() {
    let (_dir, mut fixture, project, session) = resumed_on_sonnet("opus", "xhigh");

    assert_eq!(
        fixture.backend.calls()[1..],
        [set_call(&session, "model", "opus")]
    );
    let waiting = saved(&fixture.store, &project, &session);
    assert_eq!(waiting.model, Some(ConfigValueId::new("opus")));
    assert_eq!(waiting.effort, Some(ConfigValueId::new("xhigh")));

    send(
        &mut fixture,
        &session,
        Update::ConfigOptions(agent_options("opus")),
    );

    assert_eq!(
        fixture.backend.calls()[2..],
        [
            set_call(&session, "effort", "xhigh"),
            prompt_call(&session, "again")
        ]
    );
    let applied = saved(&fixture.store, &project, &session);
    assert_eq!(applied.model, Some(ConfigValueId::new("opus")));
    assert_eq!(applied.effort, Some(ConfigValueId::new("xhigh")));

    let calls = fixture.backend.calls().len();
    send(
        &mut fixture,
        &session,
        Update::ConfigOptions(agent_options("opus")),
    );

    assert_eq!(fixture.backend.calls().len(), calls);
    let reported = saved(&fixture.store, &project, &session);
    assert_eq!(reported.model, Some(ConfigValueId::new("opus")));
    assert_eq!(reported.effort, Some(ConfigValueId::new("high")));
}

#[test]
fn a_resumed_effort_the_new_model_does_not_offer_becomes_the_agents_current_one() {
    let (_dir, mut fixture, project, session) = resumed_on_sonnet("opus", "max");

    send(
        &mut fixture,
        &session,
        Update::ConfigOptions(agent_options("opus")),
    );

    assert_eq!(
        fixture.backend.calls()[1..],
        [
            set_call(&session, "model", "opus"),
            prompt_call(&session, "again")
        ]
    );
    let resumed = saved(&fixture.store, &project, &session);
    assert_eq!(resumed.effort, Some(ConfigValueId::new("high")));

    let (_dir, mut fixture, project, session) = resumed_on_sonnet("haiku", "xhigh");

    send(
        &mut fixture,
        &session,
        Update::ConfigOptions(agent_options("haiku")),
    );

    assert_eq!(
        fixture.backend.calls()[1..],
        [
            set_call(&session, "model", "haiku"),
            prompt_call(&session, "again")
        ]
    );
    let resumed = saved(&fixture.store, &project, &session);
    assert_eq!(resumed.model, Some(ConfigValueId::new("haiku")));
    assert_eq!(resumed.effort, None);
}

#[test]
fn a_resumed_effort_on_the_agents_current_model_is_checked_at_once() {
    let (_dir, fixture, project, session) = resumed_on_sonnet("sonnet", "xhigh");

    assert_eq!(
        fixture.backend.calls()[1..],
        [prompt_call(&session, "again")]
    );
    let resumed = saved(&fixture.store, &project, &session);
    assert_eq!(resumed.effort, Some(ConfigValueId::new("high")));
}

fn rejected(option: &str, value: &str) -> Update {
    Update::ConfigRejected {
        option: ConfigId::new(option),
        value: ConfigValueId::new(value),
        error: "refused".into(),
    }
}

#[test]
fn a_rejected_model_change_releases_the_held_prompts() {
    let (_dir, mut fixture, project, session) = resumed_on_sonnet("opus", "xhigh");

    send(&mut fixture, &session, rejected("model", "opus"));

    assert_eq!(
        fixture.backend.calls()[1..],
        [
            set_call(&session, "model", "opus"),
            prompt_call(&session, "again")
        ]
    );
    let resumed = saved(&fixture.store, &project, &session);
    assert_eq!(resumed.model, Some(ConfigValueId::new("sonnet")));
    assert_eq!(resumed.effort, Some(ConfigValueId::new("high")));
    assert!(
        entries(&fixture.app).contains(&Entry::Notice("could not change model: refused".into()))
    );
    let view = fixture.app.session_view().unwrap();
    assert_eq!(view.model.unwrap().current, ConfigValueId::new("sonnet"));
    assert_eq!(view.effort.unwrap().current, ConfigValueId::new("high"));
}

#[test]
fn a_rejected_effort_change_keeps_the_agents_effort() {
    let (_dir, mut fixture, project, session) = resumed_on_sonnet("sonnet", "low");
    assert_eq!(
        fixture.backend.calls()[1..],
        [
            set_call(&session, "effort", "low"),
            prompt_call(&session, "again")
        ]
    );

    send(&mut fixture, &session, rejected("effort", "low"));

    assert_eq!(fixture.backend.calls().len(), 3);
    let resumed = saved(&fixture.store, &project, &session);
    assert_eq!(resumed.model, Some(ConfigValueId::new("sonnet")));
    assert_eq!(resumed.effort, Some(ConfigValueId::new("high")));
    assert_eq!(
        entries(&fixture.app).last(),
        Some(&Entry::Notice("could not change effort: refused".into()))
    );

    // Partitur never changes the mode: its rejection changes nothing but the transcript.
    send(&mut fixture, &session, rejected("mode", "plan"));

    assert_eq!(fixture.backend.calls().len(), 3);
    assert_eq!(
        saved(&fixture.store, &project, &session).effort,
        Some(ConfigValueId::new("high"))
    );
    assert_eq!(
        entries(&fixture.app).last(),
        Some(&Entry::Notice("could not change mode: refused".into()))
    );
}

/// [`resumed_on_sonnet`] with `opus` and `xhigh`, whose model change is on its way, and
/// with the effort `low` picked meanwhile.
fn switching_with_an_effort_picked() -> (TempDir, Fixture, ProjectId, SessionId) {
    let (dir, mut fixture, project, session) = resumed_on_sonnet("opus", "xhigh");
    fixture
        .app
        .set_effort(session.clone(), ConfigValueId::new("low"))
        .unwrap();
    assert_eq!(
        fixture.backend.calls()[1..],
        [
            set_call(&session, "model", "opus"),
            set_call(&session, "effort", "low")
        ]
    );
    (dir, fixture, project, session)
}

#[test]
fn a_rejected_model_leaves_an_effort_change_on_its_way_alone() {
    let (_dir, mut fixture, project, session) = switching_with_an_effort_picked();

    send(&mut fixture, &session, rejected("model", "opus"));

    assert_eq!(last_call(&fixture.backend), prompt_call(&session, "again"));
    let resumed = saved(&fixture.store, &project, &session);
    assert_eq!(resumed.model, Some(ConfigValueId::new("sonnet")));
    assert_eq!(resumed.effort, Some(ConfigValueId::new("low")));

    send(&mut fixture, &session, rejected("effort", "low"));

    let resumed = saved(&fixture.store, &project, &session);
    assert_eq!(resumed.effort, Some(ConfigValueId::new("high")));
    let view = fixture.app.session_view().unwrap();
    assert_eq!(view.effort.unwrap().current, ConfigValueId::new("high"));
}

#[test]
fn a_rejected_effort_keeps_the_prompts_waiting_for_the_model() {
    let (_dir, mut fixture, project, session) = switching_with_an_effort_picked();

    send(&mut fixture, &session, rejected("effort", "low"));

    assert_eq!(fixture.backend.calls().len(), 3);
    let waiting = saved(&fixture.store, &project, &session);
    assert_eq!(waiting.model, Some(ConfigValueId::new("opus")));
    assert_eq!(waiting.effort, Some(ConfigValueId::new("high")));

    send(&mut fixture, &session, rejected("model", "opus"));

    assert_eq!(last_call(&fixture.backend), prompt_call(&session, "again"));
    let resumed = saved(&fixture.store, &project, &session);
    assert_eq!(resumed.model, Some(ConfigValueId::new("sonnet")));
    assert_eq!(resumed.effort, Some(ConfigValueId::new("high")));
    let view = fixture.app.session_view().unwrap();
    assert_eq!(view.model.unwrap().current, ConfigValueId::new("sonnet"));
}

#[test]
fn a_session_deleted_meanwhile_leaves_the_changes_of_another_on_their_way() {
    let (_dir, mut fixture, project, session) = resumed_on_sonnet("opus", "xhigh");
    let other = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.delete(other, &[]).unwrap();

    send(&mut fixture, &session, rejected("model", "opus"));

    let resumed = saved(&fixture.store, &project, &session);
    assert_eq!(resumed.model, Some(ConfigValueId::new("sonnet")));
}

#[test]
fn a_rejected_change_falls_back_to_the_value_the_agent_reported_last() {
    let (_dir, mut fixture, project, session) = resumed_on_sonnet("sonnet", "high");
    fixture
        .app
        .set_model(session.clone(), ConfigValueId::new("opus"))
        .unwrap();
    send(
        &mut fixture,
        &session,
        Update::ConfigOptions(agent_options("opus")),
    );

    fixture
        .app
        .set_model(session.clone(), ConfigValueId::new("haiku"))
        .unwrap();
    fixture
        .app
        .set_model(session.clone(), ConfigValueId::new("sonnet"))
        .unwrap();
    // The rejection of an earlier pick leaves the latest one on its way.
    send(&mut fixture, &session, rejected("model", "haiku"));

    let changed = saved(&fixture.store, &project, &session);
    assert_eq!(changed.model, Some(ConfigValueId::new("sonnet")));

    send(&mut fixture, &session, rejected("model", "sonnet"));

    let changed = saved(&fixture.store, &project, &session);
    assert_eq!(changed.model, Some(ConfigValueId::new("opus")));
}

#[test]
fn a_recorded_value_the_agent_does_not_list_is_replaced_by_its_current_one() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let mut old = stored_session(&project, "old", 1);
    old.model = Some(ConfigValueId::new("opus"));
    let mut fixture = fixture_with(store_with(&project, std::slice::from_ref(&old)));
    fixture.app.select_session(old.id.clone()).unwrap();
    fixture.app.submit_prompt("again", Vec::new()).unwrap();

    fixture
        .app
        .apply_event(BackendEvent::SessionLoaded {
            request: load_request(&fixture.backend),
            session: old.id.clone(),
            config: vec![option(
                "model",
                ConfigCategory::Model,
                "sonnet",
                &[("sonnet", "Sonnet")],
            )],
        })
        .unwrap();

    assert_eq!(
        fixture.backend.calls()[1..],
        [prompt_call(&old.id, "again")]
    );
    assert_eq!(
        saved(&fixture.store, &project.id, &old.id).model,
        Some(ConfigValueId::new("sonnet"))
    );
}

#[test]
fn enabling_auto_allow_answers_requests_already_waiting() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    send(
        &mut fixture,
        &session,
        permission(
            1,
            &[
                ("reject", PermissionKind::RejectOnce),
                ("once", PermissionKind::AllowOnce),
            ],
        ),
    );
    let before = fixture.backend.calls().len();
    fixture.app.set_auto_allow(session.clone(), false).unwrap();
    assert_eq!(fixture.backend.calls().len(), before);
    assert_eq!(status(&fixture.app), Status::WaitingForYou);

    fixture.app.set_auto_allow(session.clone(), true).unwrap();
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
        saved(&fixture.store, &project, &session).status,
        Status::Running
    );

    fixture.app.set_auto_allow(session.clone(), false).unwrap();
    send(
        &mut fixture,
        &session,
        permission(2, &[("reject", PermissionKind::RejectOnce)]),
    );
    send(
        &mut fixture,
        &session,
        permission(3, &[("always", PermissionKind::AllowAlways)]),
    );
    let before = fixture.backend.calls().len();
    fixture.app.set_auto_allow(session.clone(), true).unwrap();

    assert_eq!(
        fixture.backend.calls()[before..],
        [Call::RespondPermission {
            session: session.clone(),
            permission: PermissionId(3),
            option: Some(PermissionOptionId::new("always")),
        }]
    );
    assert_eq!(status(&fixture.app), Status::WaitingForYou);
    assert_eq!(answered_updates(&fixture.store, &session).len(), 2);
}

#[test]
fn session_created_saves_the_backend_id_even_when_sending_the_queued_prompt_fails() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = fixture
        .app
        .new_session(project.clone(), Scope::Project)
        .unwrap();
    let (request, _, _) = new_session_request(&fixture.backend);
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    fixture.backend.fail_next_call();

    let result = fixture.app.apply_event(BackendEvent::SessionCreated {
        request,
        session: session.clone(),
        backend_session: BackendSessionId::new("b-1"),
        config: Vec::new(),
    });

    assert!(matches!(result, Err(AppError::Backend(_))));
    assert_eq!(
        saved(&fixture.store, &project, &session).backend_session,
        Some(BackendSessionId::new("b-1"))
    );
}

#[test]
fn a_closed_connection_fails_the_turn_and_the_next_prompt_reloads_the_session() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    let (_, _, cwd) = new_session_request(&fixture.backend);
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    send(
        &mut fixture,
        &session,
        permission(1, &[("allow", PermissionKind::AllowOnce)]),
    );
    assert_eq!(status(&fixture.app), Status::WaitingForYou);
    let before = fixture.backend.calls().len();

    fixture
        .app
        .apply_event(BackendEvent::SessionClosed {
            session: session.clone(),
            error: "the agent exited".into(),
        })
        .unwrap();

    // The backend session is gone, so the permission is settled locally only.
    assert_eq!(fixture.backend.calls().len(), before);
    assert_eq!(status(&fixture.app), Status::Failed);
    assert_eq!(
        saved(&fixture.store, &project, &session).status,
        Status::Failed
    );
    let updates = fixture.store.load_updates(session.clone()).unwrap();
    assert_eq!(
        updates[updates.len() - 2..],
        [
            Update::PermissionAnswered {
                id: PermissionId(1),
                option: None
            },
            Update::Notice("connection to the agent closed: the agent exited".into()),
        ]
    );
    assert_eq!(answer_of(&entries(&fixture.app)[1]), None);
    // The waiting request is forgotten: a late answer goes nowhere.
    fixture
        .app
        .answer_permission(session.clone(), PermissionId(1), None)
        .unwrap();
    assert_eq!(fixture.backend.calls().len(), before);

    fixture.app.submit_prompt("again", Vec::new()).unwrap();
    let request = load_request(&fixture.backend);
    assert_eq!(
        last_call(&fixture.backend),
        Call::LoadSession {
            request,
            session: session.clone(),
            backend: backend_id("claude"),
            backend_session: BackendSessionId::new("b-1"),
            cwd,
            orchestrator: false,
        }
    );
    fixture
        .app
        .apply_event(BackendEvent::SessionLoaded {
            request,
            session: session.clone(),
            config: Vec::new(),
        })
        .unwrap();
    assert_eq!(last_call(&fixture.backend), prompt_call(&session, "again"));
    assert_eq!(status(&fixture.app), Status::Running);
}

#[test]
fn a_closed_connection_leaves_a_finished_session_alone_but_reloads_it() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    send(
        &mut fixture,
        &session,
        Update::TurnEnded(StopReason::EndTurn),
    );

    fixture
        .app
        .apply_event(BackendEvent::SessionClosed {
            session: session.clone(),
            error: "the agent exited".into(),
        })
        .unwrap();

    assert_eq!(status(&fixture.app), Status::Done);
    assert_eq!(
        entries(&fixture.app).last(),
        Some(&Entry::Notice(
            "connection to the agent closed: the agent exited".into()
        ))
    );
    fixture.app.submit_prompt("next", Vec::new()).unwrap();
    assert!(matches!(
        last_call(&fixture.backend),
        Call::LoadSession { .. }
    ));
}

const SWITCHED_TO_B: &str = "switched to b; the new agent has no memory of earlier turns";

/// A fixture on backends `a` and `b`, defaulting to `a`, holding `sessions` of `project`.
fn fixture_a_and_b(project: &Project, sessions: &[Session]) -> Fixture {
    fixture_on(
        store_with(project, sessions),
        backends_a_and_b(),
        backend_id("a"),
    )
}

#[test]
fn switching_a_session_without_entries_creates_on_the_new_backend_without_a_notice() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let mut old = stored_session(&project, "old", 1);
    old.backend = backend_id("a");
    old.backend_session = None;
    let mut fixture = fixture_a_and_b(&project, std::slice::from_ref(&old));
    fixture.app.select_session(old.id.clone()).unwrap();

    fixture
        .app
        .switch_backend(old.id.clone(), backend_id("b"))
        .unwrap();

    let stamped = saved(&fixture.store, &project.id, &old.id).last_activity;
    let expected = Session {
        backend: backend_id("b"),
        last_activity: stamped,
        ..old.clone()
    };
    assert!(stamped > 0);
    assert_eq!(saved(&fixture.store, &project.id, &old.id), expected);
    assert_eq!(fixture.app.sessions, [expected]);
    assert!(entries(&fixture.app).is_empty());
    assert!(
        fixture
            .store
            .load_updates(old.id.clone())
            .unwrap()
            .is_empty()
    );
    let (request, _, _) = new_session_request(&fixture.backend);
    assert_eq!(
        fixture.backend.calls(),
        [Call::NewSession {
            request,
            session: old.id.clone(),
            backend: backend_id("b"),
            cwd: project.root.clone(),
            orchestrator: false,
        }]
    );
    // A create on its way is not a running turn.
    assert_eq!(status(&fixture.app), Status::Idle);
    assert!(!fixture.app.session_view().unwrap().running());
}

#[test]
fn switching_a_session_with_entries_notices_that_the_new_agent_forgets_them() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let mut old = stored_session(&project, "old", 1);
    old.backend = backend_id("a");
    let store = store_with(&project, std::slice::from_ref(&old));
    store
        .append_update(old.id.clone(), &Update::UserText("hi".into()))
        .unwrap();
    let mut fixture = fixture_on(store, backends_a_and_b(), backend_id("a"));

    // Not selected, so its transcript is read from the store to see that it has entries.
    fixture
        .app
        .switch_backend(old.id.clone(), backend_id("b"))
        .unwrap();

    assert_eq!(
        fixture.store.load_updates(old.id.clone()).unwrap(),
        [
            Update::UserText("hi".into()),
            Update::Notice(SWITCHED_TO_B.into())
        ]
    );
    fixture.app.select_session(old.id.clone()).unwrap();
    assert_eq!(
        entries(&fixture.app),
        [
            Entry::User("hi".into()),
            Entry::Notice(SWITCHED_TO_B.into())
        ]
    );
    // The stored backend session belongs to the old backend: a create, not a load.
    assert_eq!(
        saved(&fixture.store, &project.id, &old.id).backend_session,
        None
    );
    assert!(matches!(
        last_call(&fixture.backend),
        Call::NewSession { backend, .. } if backend == backend_id("b")
    ));
}

#[test]
fn a_switch_whose_transcript_cannot_be_read_changes_nothing() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let mut old = stored_session(&project, "old", 1);
    old.backend = backend_id("a");
    let mut fixture = fixture_a_and_b(&project, std::slice::from_ref(&old));
    fixture.store.set_corrupt_updates(true);

    let switched = fixture.app.switch_backend(old.id.clone(), backend_id("b"));

    assert!(matches!(switched, Err(AppError::Store(_))));
    assert_eq!(saved(&fixture.store, &project.id, &old.id), old);
    assert_eq!(fixture.app.sessions, [old]);
    assert!(fixture.backend.calls().is_empty());
}

#[test]
fn a_switch_whose_create_cannot_be_queued_is_kept_and_the_next_prompt_creates_again() {
    let dir = project_dir(&[]);
    let mut fixture = fixture_on(MemoryStore::new(), backends_a_and_b(), backend_id("a"));
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.backend.fail_next_call();

    let switched = fixture.app.switch_backend(session.clone(), backend_id("b"));

    assert!(matches!(
        switched,
        Err(AppError::Backend(BackendError::Unavailable(_)))
    ));
    let stored = saved(&fixture.store, &project, &session);
    assert_eq!(stored.backend, backend_id("b"));
    assert_eq!(stored.backend_session, None);
    assert_eq!(stored.status, Status::Idle);
    assert!(fixture.app.pending.is_empty());

    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    assert!(matches!(
        last_call(&fixture.backend),
        Call::NewSession { backend, .. } if backend == backend_id("b")
    ));
    assert_eq!(status(&fixture.app), Status::Running);
}

#[test]
fn switching_to_an_unconfigured_backend_or_an_unknown_session_is_refused() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let old = stored_session(&project, "old", 1);
    let mut fixture = fixture_a_and_b(&project, std::slice::from_ref(&old));
    let ghost = SessionId::new();

    let unconfigured = fixture.app.switch_backend(old.id.clone(), backend_id("c"));
    let unknown = fixture.app.switch_backend(ghost.clone(), backend_id("b"));

    assert!(matches!(unconfigured, Err(AppError::UnknownBackend(id)) if id == backend_id("c")));
    assert!(matches!(unknown, Err(AppError::UnknownSession(id)) if id == ghost));
    assert_eq!(saved(&fixture.store, &project.id, &old.id), old);
    assert!(fixture.store.load_updates(old.id).unwrap().is_empty());
}

#[test]
fn switching_is_refused_while_running_or_waiting() {
    let dir = project_dir(&[]);
    let mut fixture = fixture_on(MemoryStore::new(), backends_a_and_b(), backend_id("a"));
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();

    let running = fixture.app.switch_backend(session.clone(), backend_id("b"));
    send(
        &mut fixture,
        &session,
        permission(1, &[("allow", PermissionKind::AllowOnce)]),
    );
    let waiting = fixture.app.switch_backend(session.clone(), backend_id("b"));

    assert!(matches!(running, Err(AppError::TurnInProgress)));
    assert!(matches!(waiting, Err(AppError::TurnInProgress)));
    let stored = saved(&fixture.store, &project, &session);
    assert_eq!(stored.backend, backend_id("a"));
    assert_eq!(stored.backend_session, Some(BackendSessionId::new("b-1")));
    assert_eq!(stored.status, Status::WaitingForYou);
    assert!(
        !entries(&fixture.app)
            .iter()
            .any(|entry| matches!(entry, Entry::Notice(_)))
    );
}

#[test]
fn switching_to_the_same_backend_is_a_no_op() {
    let dir = project_dir(&[]);
    let mut fixture = fixture_on(MemoryStore::new(), backends_a_and_b(), backend_id("a"));
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    let before = saved(&fixture.store, &project, &session);

    fixture
        .app
        .switch_backend(session.clone(), backend_id("a"))
        .unwrap();

    assert_eq!(saved(&fixture.store, &project, &session), before);
    assert!(
        fixture
            .store
            .load_updates(session.clone())
            .unwrap()
            .is_empty()
    );
    // Still attached: the next prompt goes straight to the backend session.
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    assert!(matches!(last_call(&fixture.backend), Call::Prompt { .. }));
}

#[test]
fn switching_an_attached_session_detaches_and_creates_on_the_new_backend_at_once() {
    let dir = project_dir(&[]);
    fs::write(dir.path().join("AGENTS.md"), "rules").unwrap();
    let mut fixture = fixture_on(MemoryStore::new(), backends_a_and_b(), backend_id("a"));
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    let (_, _, cwd) = new_session_request(&fixture.backend);
    fixture.app.submit_prompt("one", Vec::new()).unwrap();
    send(
        &mut fixture,
        &session,
        Update::TurnEnded(StopReason::EndTurn),
    );

    fixture
        .app
        .switch_backend(session.clone(), backend_id("b"))
        .unwrap();

    let stored = saved(&fixture.store, &project, &session);
    assert_eq!(stored.backend, backend_id("b"));
    assert_eq!(stored.backend_session, None);
    assert!(!stored.instructions_injected);
    assert_eq!(stored.status, Status::Idle);
    assert_eq!(status(&fixture.app), Status::Idle);
    assert_eq!(
        entries(&fixture.app).last(),
        Some(&Entry::Notice(SWITCHED_TO_B.into()))
    );
    let (request, _, _) = new_session_request(&fixture.backend);
    assert_eq!(
        last_call(&fixture.backend),
        Call::NewSession {
            request,
            session: session.clone(),
            backend: backend_id("b"),
            cwd,
            orchestrator: false,
        }
    );

    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: session.clone(),
            backend_session: BackendSessionId::new("b-2"),
            config: Vec::new(),
        })
        .unwrap();
    assert_eq!(status(&fixture.app), Status::Idle);
    assert_eq!(
        saved(&fixture.store, &project, &session).backend_session,
        Some(BackendSessionId::new("b-2"))
    );

    // The new agent is attached, so the prompt goes straight to it, with the instructions.
    fixture.app.submit_prompt("two", Vec::new()).unwrap();
    let discovered = discover_project(dir.path()).unwrap();
    let chain = instruction_chain(&discovered, None);
    assert_eq!(
        last_call(&fixture.backend),
        prompt_call(&session, &inject(&chain, "two"))
    );
}

#[test]
fn a_late_creation_from_the_old_backend_delivers_no_prompt_after_a_switch() {
    let dir = project_dir(&[]);
    let mut fixture = fixture_on(MemoryStore::new(), backends_a_and_b(), backend_id("a"));
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = fixture
        .app
        .new_session(project.clone(), Scope::Project)
        .unwrap();
    let (old_request, _, _) = new_session_request(&fixture.backend);

    fixture
        .app
        .switch_backend(session.clone(), backend_id("b"))
        .unwrap();
    let (request, _, _) = new_session_request(&fixture.backend);
    assert_ne!(request, old_request);
    // The prompt waits for the new backend's create, which is already on its way.
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    let calls = fixture.backend.calls().len();
    assert!(matches!(
        last_call(&fixture.backend),
        Call::NewSession { backend, .. } if backend == backend_id("b")
    ));

    // The old backend's answer comes too late: it neither attaches nor takes the prompt.
    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request: old_request,
            session: session.clone(),
            backend_session: BackendSessionId::new("old"),
            config: Vec::new(),
        })
        .unwrap();

    assert_eq!(fixture.backend.calls().len(), calls);
    assert!(!fixture.app.attached.contains(&session));
    assert_eq!(
        saved(&fixture.store, &project, &session).backend_session,
        None
    );
    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: session.clone(),
            backend_session: BackendSessionId::new("new"),
            config: Vec::new(),
        })
        .unwrap();
    assert_eq!(fixture.backend.calls().len(), calls + 1);
    assert!(matches!(last_call(&fixture.backend), Call::Prompt { .. }));
    assert_eq!(
        saved(&fixture.store, &project, &session).backend_session,
        Some(BackendSessionId::new("new"))
    );
}

#[test]
fn switching_closes_waiting_permissions() {
    let dir = project_dir(&[]);
    let mut fixture = fixture_on(MemoryStore::new(), backends_a_and_b(), backend_id("a"));
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    send(
        &mut fixture,
        &session,
        permission(1, &[("allow", PermissionKind::AllowOnce)]),
    );
    // Every event that ends a turn settles its permission requests; force a finished
    // session that still has one, so switching is shown to close it all the same.
    fixture.app.sessions[0].status = Status::Failed;

    fixture
        .app
        .switch_backend(session.clone(), backend_id("b"))
        .unwrap();

    let updates = fixture.store.load_updates(session.clone()).unwrap();
    assert_eq!(
        updates[updates.len() - 2..],
        [
            Update::PermissionAnswered {
                id: PermissionId(1),
                option: None
            },
            Update::Notice(SWITCHED_TO_B.into()),
        ]
    );
    assert_eq!(answer_of(&entries(&fixture.app)[0]), None);
    assert_eq!(status(&fixture.app), Status::Idle);
    // The agent is gone, so the backend is not told; a late answer goes nowhere. The only
    // calls are the two creates, on `a` and then on `b`.
    fixture
        .app
        .answer_permission(session.clone(), PermissionId(1), None)
        .unwrap();
    assert_eq!(fixture.backend.calls().len(), 2);
    assert!(matches!(
        last_call(&fixture.backend),
        Call::NewSession { backend, .. } if backend == backend_id("b")
    ));
}

#[test]
fn prompting_a_session_on_a_removed_backend_is_an_error() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let mut old = stored_session(&project, "old", 1);
    old.backend = backend_id("gone");
    let mut fixture = fixture_a_and_b(&project, std::slice::from_ref(&old));
    fixture.app.select_session(old.id.clone()).unwrap();

    let prompt = fixture.app.submit_prompt("hi", Vec::new());
    let blank = fixture.app.submit_prompt("", Vec::new());

    assert!(matches!(prompt, Err(AppError::UnknownBackend(id)) if id == backend_id("gone")));
    assert!(matches!(blank, Err(AppError::UnknownBackend(id)) if id == backend_id("gone")));
    assert!(fixture.backend.calls().is_empty());
    assert!(entries(&fixture.app).is_empty());
    assert_eq!(status(&fixture.app), Status::Idle);
}

#[test]
fn late_events_for_a_forgotten_request_are_ignored() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    let before = saved(&fixture.store, &project, &session);
    let forgotten = RequestId(99);

    let events = [
        BackendEvent::SessionCreated {
            request: forgotten,
            session: session.clone(),
            backend_session: BackendSessionId::new("other"),
            config: vec![model_option()],
        },
        BackendEvent::SessionLoaded {
            request: forgotten,
            session: session.clone(),
            config: vec![model_option()],
        },
        BackendEvent::RequestFailed {
            request: forgotten,
            session: session.clone(),
            error: "boom".into(),
        },
    ];
    for event in events {
        fixture.app.apply_event(event).unwrap();
    }

    assert_eq!(saved(&fixture.store, &project, &session), before);
    assert_eq!(fixture.app.session_view().unwrap().model, None);
    assert_eq!(status(&fixture.app), Status::Idle);
    assert!(fixture.store.load_updates(session).unwrap().is_empty());
}

#[test]
fn the_view_models_carry_backends_and_command() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let mut on_a = stored_session(&project, "on a", 1);
    on_a.backend = backend_id("a");
    let mut on_gone = stored_session(&project, "on gone", 2);
    on_gone.backend = backend_id("gone");
    let mut fixture = fixture_a_and_b(&project, &[on_a.clone(), on_gone.clone()]);

    let tree = fixture.app.tree();
    let rows: Vec<(&str, &str)> = tree.projects[0]
        .sessions
        .iter()
        .map(|node| (node.title.as_str(), node.backend.as_str()))
        .collect();
    assert_eq!(rows, [("on gone", "gone"), ("on a", "a")]);

    fixture.app.select_session(on_a.id.clone()).unwrap();
    let view = fixture.app.session_view().unwrap();
    assert_eq!(view.backends, backends_a_and_b());
    assert_eq!(view.backend, backend_id("a"));
    let env = fixture.app.env_view().unwrap();
    assert_eq!(env.backend, "a");
    assert_eq!(env.command.as_deref(), Some("agent-a"));

    fixture.app.select_session(on_gone.id.clone()).unwrap();
    let view = fixture.app.session_view().unwrap();
    assert_eq!(view.backends, backends_a_and_b());
    assert_eq!(view.backend, backend_id("gone"));
    let env = fixture.app.env_view().unwrap();
    assert_eq!(env.backend, "gone");
    assert_eq!(env.command, None);
}

#[test]
fn switching_forgets_the_old_backends_options() {
    let dir = project_dir(&[]);
    let mut fixture = fixture_on(MemoryStore::new(), backends_a_and_b(), backend_id("a"));
    let project = fixture.app.open_project(dir.path()).unwrap();
    let models = [("fast", "Fast"), ("opus", "Opus")];
    let efforts = [("low", "Low"), ("high", "High")];
    let config = || {
        vec![
            option("model", ConfigCategory::Model, "fast", &models),
            option("effort", ConfigCategory::Effort, "low", &efforts),
        ]
    };
    let session = created_session(&mut fixture, &project, Scope::Project, config());
    fixture
        .app
        .set_model(session.clone(), ConfigValueId::new("opus"))
        .unwrap();
    fixture
        .app
        .set_effort(session.clone(), ConfigValueId::new("high"))
        .unwrap();

    fixture
        .app
        .switch_backend(session.clone(), backend_id("b"))
        .unwrap();

    let view = fixture.app.session_view().unwrap();
    assert_eq!(view.model, None);
    assert_eq!(view.effort, None);
    let stored = saved(&fixture.store, &project, &session);
    assert_eq!(stored.model, None);
    assert_eq!(stored.effort, None);
    // The old agent is no longer reachable through its options.
    let calls = fixture.backend.calls().len();
    let model = fixture
        .app
        .set_model(session.clone(), ConfigValueId::new("opus"));
    let effort = fixture
        .app
        .set_effort(session.clone(), ConfigValueId::new("high"));
    assert!(matches!(
        model,
        Err(AppError::NoSuchConfig(ConfigCategory::Model))
    ));
    assert!(matches!(
        effort,
        Err(AppError::NoSuchConfig(ConfigCategory::Effort))
    ));
    assert_eq!(fixture.backend.calls().len(), calls);

    // The new backend was asked for a session at the switch; its own current values stand
    // as soon as it reports them, and nothing is re-applied from the old one.
    let (request, _, _) = new_session_request(&fixture.backend);
    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: session.clone(),
            backend_session: BackendSessionId::new("b-2"),
            config: config(),
        })
        .unwrap();
    assert_eq!(fixture.backend.calls().len(), calls);
    assert!(matches!(
        last_call(&fixture.backend),
        Call::NewSession { .. }
    ));
    let view = fixture.app.session_view().unwrap();
    assert_eq!(view.model.unwrap().current, ConfigValueId::new("fast"));
    assert_eq!(view.effort.unwrap().current, ConfigValueId::new("low"));
    assert_eq!(status(&fixture.app), Status::Idle);
}

#[test]
fn events_from_a_detached_backend_are_ignored() {
    let dir = project_dir(&[]);
    let mut fixture = fixture_on(MemoryStore::new(), backends_a_and_b(), backend_id("a"));
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture
        .app
        .switch_backend(session.clone(), backend_id("b"))
        .unwrap();
    let before = saved(&fixture.store, &project, &session);
    let updates = fixture.store.load_updates(session.clone()).unwrap();

    // The old backend's connection is still winding down and reports late.
    fixture
        .app
        .apply_event(BackendEvent::Update {
            session: session.clone(),
            update: Update::ConfigOptions(vec![model_option()]),
        })
        .unwrap();
    fixture
        .app
        .apply_event(BackendEvent::SessionClosed {
            session: session.clone(),
            error: "the agent exited".into(),
        })
        .unwrap();

    let view = fixture.app.session_view().unwrap();
    assert_eq!(view.model, None);
    assert_eq!(view.status, Status::Idle);
    assert!(entries(&fixture.app).is_empty());
    assert_eq!(saved(&fixture.store, &project, &session), before);
    assert_eq!(fixture.store.load_updates(session).unwrap(), updates);
}

#[test]
fn a_session_being_created_still_receives_its_creation() {
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
    send(&mut fixture, &session, Update::AgentText("hello".into()));

    assert!(fixture.app.attached.contains(&session));
    assert_eq!(entries(&fixture.app), [Entry::Agent("hello".into())]);
    assert_eq!(
        fixture.app.session_view().unwrap().model.unwrap().current,
        ConfigValueId::new("fast")
    );
}

#[test]
fn a_project_session_in_a_two_repository_project_is_an_orchestrator() {
    let two = project_dir(&["api", "web"]);
    let one = project_dir(&["api"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(two.path()).unwrap();
    let single = fixture.app.open_project(one.path()).unwrap();

    let orchestrator = fixture
        .app
        .new_session(project.clone(), Scope::Project)
        .unwrap();
    let orchestrator_call = last_call(&fixture.backend);
    let plain = fixture
        .app
        .new_session(single.clone(), Scope::Project)
        .unwrap();
    let plain_call = last_call(&fixture.backend);

    assert!(fixture.app.session(&orchestrator).unwrap().orchestrator);
    assert!(saved(&fixture.store, &project, &orchestrator).orchestrator);
    assert!(matches!(
        orchestrator_call,
        Call::NewSession {
            orchestrator: true,
            ..
        }
    ));
    assert!(!fixture.app.session(&plain).unwrap().orchestrator);
    assert!(!saved(&fixture.store, &single, &plain).orchestrator);
    assert!(matches!(
        plain_call,
        Call::NewSession {
            orchestrator: false,
            ..
        }
    ));
}

#[test]
fn the_first_orchestrator_prompt_carries_the_preamble_after_the_global_instructions() {
    let dir = project_dir(&["api", "web"]);
    fs::write(dir.path().join("AGENTS.md"), "global rules").unwrap();
    fs::write(dir.path().join("api/AGENTS.md"), "Keep the API compatible.").unwrap();
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    let opened = fixture.app.project(&project).unwrap().clone();
    let chain = instruction_chain(&opened, None);
    assert_eq!(chain.files.len(), 1);

    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
    let first = last_call(&fixture.backend);
    send(
        &mut fixture,
        &session,
        Update::TurnEnded(StopReason::EndTurn),
    );
    fixture.app.submit_prompt("Next", Vec::new()).unwrap();

    let text = inject(&chain, &format!("{}\n\nPlan it", preamble(&opened)));
    assert_eq!(first, prompt_call(&session, &text));
    assert_eq!(last_call(&fixture.backend), prompt_call(&session, "Next"));
    assert_eq!(entries(&fixture.app)[0], Entry::User("Plan it".into()));
}

#[test]
fn changing_permission_requests_are_denied_in_orchestrator_sessions() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    let options = [
        ("allow", PermissionKind::AllowOnce),
        ("reject", PermissionKind::RejectOnce),
    ];
    fixture.app.submit_prompt("go", Vec::new()).unwrap();

    send(
        &mut fixture,
        &session,
        permission_for(1, "Run cargo test", ToolKind::Execute, None, &options),
    );

    assert_eq!(
        last_call(&fixture.backend),
        Call::RespondPermission {
            session: session.clone(),
            permission: PermissionId(1),
            option: Some(PermissionOptionId::new("reject")),
        }
    );
    assert_eq!(
        entries(&fixture.app).last(),
        Some(&Entry::Notice(
            "denied: an orchestrator session may inspect but never change; delegate \"Run cargo test\" (execute) to a child session".into()
        ))
    );
    assert_eq!(status(&fixture.app), Status::Running);

    send(
        &mut fixture,
        &session,
        permission_for(2, "Read main.rs", ToolKind::Read, None, &options),
    );

    assert_eq!(status(&fixture.app), Status::WaitingForYou);
}

#[test]
fn a_restrained_request_is_denied_even_with_auto_allow_on() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.set_auto_allow(session.clone(), true).unwrap();
    let options = [
        ("allow-once", PermissionKind::AllowOnce),
        ("reject", PermissionKind::RejectOnce),
    ];
    fixture.app.submit_prompt("go", Vec::new()).unwrap();

    send(
        &mut fixture,
        &session,
        permission_for(1, "Run cargo test", ToolKind::Execute, None, &options),
    );

    assert_eq!(
        last_call(&fixture.backend),
        Call::RespondPermission {
            session: session.clone(),
            permission: PermissionId(1),
            option: Some(PermissionOptionId::new("reject")),
        }
    );
    assert_eq!(
        entries(&fixture.app).last(),
        Some(&Entry::Notice(denial_notice("execute", "Run cargo test")))
    );

    send(
        &mut fixture,
        &session,
        permission_for(2, "Read main.rs", ToolKind::Read, None, &options),
    );

    assert_eq!(
        last_call(&fixture.backend),
        Call::RespondPermission {
            session,
            permission: PermissionId(2),
            option: Some(PermissionOptionId::new("allow-once")),
        }
    );
    assert_eq!(status(&fixture.app), Status::Running);
}

#[test]
fn a_restrained_request_without_a_reject_option_is_cancelled() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();

    send(
        &mut fixture,
        &session,
        permission_for(
            1,
            "Edit main.rs",
            ToolKind::Edit,
            None,
            &[("allow", PermissionKind::AllowOnce)],
        ),
    );

    assert_eq!(
        last_call(&fixture.backend),
        Call::RespondPermission {
            session,
            permission: PermissionId(1),
            option: None,
        }
    );
}

#[test]
fn partitur_tool_calls_are_allowed_without_asking() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    assert!(!fixture.app.session(&session).unwrap().auto_allow);
    fixture.app.submit_prompt("go", Vec::new()).unwrap();

    let mut request = permission_for(
        1,
        "propose_children",
        ToolKind::Other,
        Some("mcp__partitur__propose_children"),
        &[
            ("reject", PermissionKind::RejectOnce),
            ("allow", PermissionKind::AllowOnce),
        ],
    );
    if let Update::PermissionRequested { partitur_tool, .. } = &mut request {
        *partitur_tool = true;
    }
    send(&mut fixture, &session, request);

    assert_eq!(
        last_call(&fixture.backend),
        Call::RespondPermission {
            session,
            permission: PermissionId(1),
            option: Some(PermissionOptionId::new("allow")),
        }
    );
    assert_eq!(status(&fixture.app), Status::Running);
}

#[test]
fn a_partitur_named_request_without_the_flag_is_restrained() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();

    send(
        &mut fixture,
        &session,
        permission_for(
            1,
            "propose_children",
            ToolKind::Edit,
            Some("mcp__partitur__propose_children"),
            &[
                ("allow", PermissionKind::AllowOnce),
                ("reject", PermissionKind::RejectOnce),
            ],
        ),
    );

    assert_eq!(
        last_call(&fixture.backend),
        Call::RespondPermission {
            session,
            permission: PermissionId(1),
            option: Some(PermissionOptionId::new("reject")),
        }
    );
    assert_eq!(
        entries(&fixture.app).last(),
        Some(&Entry::Notice(denial_notice("edit", "propose_children")))
    );
}

#[test]
fn repository_sessions_are_not_restrained() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let api = RepositoryId::try_from("api").unwrap();
    let session = created_session(&mut fixture, &project, Scope::Repository(api), Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    let calls = fixture.backend.calls().len();

    send(
        &mut fixture,
        &session,
        permission_for(
            1,
            "Run cargo test",
            ToolKind::Execute,
            None,
            &[
                ("allow", PermissionKind::AllowOnce),
                ("reject", PermissionKind::RejectOnce),
            ],
        ),
    );

    assert_eq!(fixture.backend.calls().len(), calls);
    assert_eq!(status(&fixture.app), Status::WaitingForYou);
}

#[test]
fn agent_turn_makes_a_done_session_running() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    send(
        &mut fixture,
        &session,
        Update::TurnEnded(StopReason::EndTurn),
    );
    assert_eq!(status(&fixture.app), Status::Done);

    send(&mut fixture, &session, Update::TurnStarted);

    assert_eq!(status(&fixture.app), Status::Running);
    assert_eq!(entries(&fixture.app).last(), Some(&Entry::AgentTurn));
    assert_eq!(
        fixture.store.load_updates(session).unwrap().last(),
        Some(&Update::TurnStarted)
    );
}

#[test]
fn turn_started_during_a_prompted_turn_records_nothing() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    let before = entries(&fixture.app);
    let updates = fixture.store.load_updates(session.clone()).unwrap();

    send(&mut fixture, &session, Update::TurnStarted);

    assert_eq!(status(&fixture.app), Status::Running);
    assert_eq!(entries(&fixture.app), before);
    assert_eq!(fixture.store.load_updates(session).unwrap(), updates);
}

#[test]
fn user_prompt_during_agent_turn_is_refused() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    send(
        &mut fixture,
        &session,
        Update::TurnEnded(StopReason::EndTurn),
    );
    send(&mut fixture, &session, Update::TurnStarted);

    let refused = fixture.app.submit_prompt("more", Vec::new());

    assert!(matches!(refused, Err(AppError::TurnInProgress)));
}

#[test]
fn a_status_change_stamps_last_activity() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    let before = now_seconds();

    fixture.app.submit_prompt("go", Vec::new()).unwrap();

    let stored = saved(&fixture.store, &project, &session);
    assert!(stored.last_activity >= before);
    assert!(stored.active_at() >= before);
}

// Archiving and restoring sessions.

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn archived(app: &App, id: &SessionId) -> Option<Archived> {
    app.sessions.iter().find(|s| &s.id == id).unwrap().archived
}

/// An open project with a session that finished a turn.
fn done_session(fixture: &mut Fixture, dir: &TempDir) -> (ProjectId, SessionId) {
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    send(fixture, &session, Update::TurnEnded(StopReason::EndTurn));
    (project, session)
}

#[test]
fn archiving_a_done_session_keeps_its_status_and_saves_the_mark() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let (project, session) = done_session(&mut fixture, &dir);
    let before = now_seconds();

    fixture.app.archive(session.clone()).unwrap();

    let stored = saved(&fixture.store, &project, &session);
    assert!(stored.archived.is_some_and(|mark| mark.at >= before));
    assert!(stored.archived.is_some_and(|mark| mark.at <= now_seconds()));
    assert_eq!(stored.status, Status::Done);
    assert_eq!(fixture.app.current_session(), Some(session));
}

#[test]
fn the_live_sessions_leave_out_the_archived_ones() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let (project, archived) = done_session(&mut fixture, &dir);
    let live = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    assert_eq!(
        fixture.app.live_sessions(),
        [archived.clone(), live.clone()]
    );

    fixture.app.archive(archived).unwrap();

    assert_eq!(fixture.app.live_sessions(), [live]);
}

#[test]
fn archiving_a_running_session_stops_its_turn() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();

    fixture.app.archive(session.clone()).unwrap();

    assert_eq!(
        last_call(&fixture.backend),
        Call::Cancel {
            session: session.clone()
        }
    );
    assert!(archived(&fixture.app, &session).is_some());
    send(
        &mut fixture,
        &session,
        Update::TurnEnded(StopReason::Cancelled),
    );
    assert_eq!(session_status(&fixture.app, &session), Status::Done);
    assert!(archived(&fixture.app, &session).is_some());
}

#[test]
fn archiving_a_session_that_waits_for_a_permission_answers_it_first() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    send(
        &mut fixture,
        &session,
        permission(1, &[("allow", PermissionKind::AllowOnce)]),
    );
    let before = fixture.backend.calls().len();

    fixture.app.archive(session.clone()).unwrap();

    assert_eq!(
        fixture.backend.calls()[before..],
        [
            Call::RespondPermission {
                session: session.clone(),
                permission: PermissionId(1),
                option: None,
            },
            Call::Cancel { session },
        ]
    );
}

#[test]
fn a_failed_stop_leaves_the_family_live() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    let saves = fixture.store.session_saves(&project);

    fixture.backend.fail_next_call();
    let refused = fixture.app.archive(session.clone());

    assert!(matches!(
        refused,
        Err(AppError::Backend(BackendError::Unavailable(_)))
    ));
    assert_eq!(archived(&fixture.app, &session), None);
    assert_eq!(saved(&fixture.store, &project, &session).archived, None);
    assert_eq!(fixture.store.session_saves(&project), saves + 1);
}

#[test]
fn an_unknown_backend_session_does_not_block_archiving() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();

    fixture
        .backend
        .fail_next_call_with(BackendError::UnknownSession(session.clone()));
    fixture.app.archive(session.clone()).unwrap();

    assert!(saved(&fixture.store, &project, &session).archived.is_some());
}

#[test]
fn an_archived_session_takes_no_prompt() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let (_, session) = done_session(&mut fixture, &dir);
    fixture.app.archive(session.clone()).unwrap();
    let before = fixture.backend.calls().len();

    let refused = fixture.app.submit_prompt("more", Vec::new());

    assert!(matches!(refused, Err(AppError::Archived(id)) if id == session));
    assert_eq!(fixture.backend.calls().len(), before);
}

#[test]
fn archiving_twice_is_refused() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let (_, session) = done_session(&mut fixture, &dir);
    fixture.app.archive(session.clone()).unwrap();
    let first = archived(&fixture.app, &session);

    let refused = fixture.app.archive(session.clone());

    assert!(matches!(refused, Err(AppError::Archived(id)) if id == session));
    assert_eq!(archived(&fixture.app, &session), first);
}

#[test]
fn archiving_or_restoring_an_unknown_session_fails() {
    let mut fixture = fixture();
    let unknown = SessionId::new();

    assert!(matches!(
        fixture.app.archive(unknown.clone()),
        Err(AppError::UnknownSession(id)) if id == unknown
    ));
    assert!(matches!(
        fixture.app.restore(unknown.clone()),
        Err(AppError::UnknownSession(id)) if id == unknown
    ));
}

#[test]
fn restoring_brings_back_the_previous_status() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    send(&mut fixture, &session, Update::Failed("broke".into()));
    fixture.app.archive(session.clone()).unwrap();

    fixture.app.restore(session.clone()).unwrap();

    assert_eq!(archived(&fixture.app, &session), None);
    let stored = saved(&fixture.store, &project, &session);
    assert_eq!(stored.archived, None);
    assert_eq!(stored.status, Status::Failed);
    fixture.app.submit_prompt("again", Vec::new()).unwrap();
}

#[test]
fn restoring_a_live_session_changes_nothing() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let (project, session) = done_session(&mut fixture, &dir);
    let saves = fixture.store.session_saves(&project);

    fixture.app.restore(session.clone()).unwrap();

    assert_eq!(fixture.store.session_saves(&project), saves);
    assert_eq!(archived(&fixture.app, &session), None);
}

#[test]
fn an_archived_session_stays_archived_after_reload() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let (_, session) = done_session(&mut fixture, &dir);
    fixture.app.archive(session.clone()).unwrap();
    let mark = archived(&fixture.app, &session);

    let reloaded = fixture_with(fixture.store.clone());

    assert!(mark.is_some());
    assert_eq!(archived(&reloaded.app, &session), mark);
}

#[test]
fn session_and_env_models_tell_that_the_session_is_archived() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let (_, session) = done_session(&mut fixture, &dir);
    assert_eq!(fixture.app.session_view().unwrap().archived, None);
    assert_eq!(fixture.app.env_view().unwrap().archived, None);

    fixture.app.archive(session.clone()).unwrap();

    let mark = archived(&fixture.app, &session);
    assert!(mark.is_some());
    assert_eq!(fixture.app.session_view().unwrap().archived, mark);
    assert_eq!(fixture.app.env_view().unwrap().archived, mark);
}

#[test]
fn the_archive_summary_of_a_done_session() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let (_, session) = done_session(&mut fixture, &dir);

    let summary = fixture.app.archive_summary(&session).unwrap();

    assert_eq!(
        summary,
        SessionSummary {
            id: session,
            title: "untitled".into(),
            project: dir
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            orchestrator: false,
            children: 0,
            status: Status::Done,
            stops: false,
            cleanup: Vec::new(),
        }
    );
}

#[test]
fn the_archive_summary_tells_when_a_waiting_session_is_stopped() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    send(
        &mut fixture,
        &session,
        permission(1, &[("allow", PermissionKind::AllowOnce)]),
    );

    let summary = fixture.app.archive_summary(&session).unwrap();

    assert!(summary.stops);
    assert_eq!(summary.status, Status::WaitingForYou);
}

#[test]
fn the_archive_summary_refuses_what_archiving_refuses() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let (_, session) = done_session(&mut fixture, &dir);
    let unknown = SessionId::new();
    fixture.app.archive(session.clone()).unwrap();

    assert!(matches!(
        fixture.app.archive_summary(&session),
        Err(AppError::Archived(id)) if id == session
    ));
    assert!(matches!(
        fixture.app.archive_summary(&unknown),
        Err(AppError::UnknownSession(id)) if id == unknown
    ));
}

// Deleting sessions.

/// The sessions the backend was asked to close, in order.
fn closed(backend: &RecordingBackend) -> Vec<SessionId> {
    backend
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            Call::CloseSession { session } => Some(session),
            _ => None,
        })
        .collect()
}

/// The ids of the sessions the store holds for `project`.
fn stored_ids(store: &MemoryStore, project: &ProjectId) -> Vec<SessionId> {
    store
        .load_sessions(project.clone())
        .unwrap()
        .into_iter()
        .map(|session| session.id)
        .collect()
}

#[test]
fn deleting_a_session_removes_it_and_its_transcript_and_closes_it() {
    let dir = project_dir(&[]);
    let elsewhere = project_dir(&[]);
    let mut fixture = fixture();
    let other_project = fixture.app.open_project(elsewhere.path()).unwrap();
    let unrelated = created_session(&mut fixture, &other_project, Scope::Project, Vec::new());
    let project = fixture.app.open_project(dir.path()).unwrap();
    let kept = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    let (_, session) = done_session(&mut fixture, &dir);
    assert!(
        !fixture
            .store
            .load_updates(session.clone())
            .unwrap()
            .is_empty()
    );

    fixture.app.delete(session.clone(), &[]).unwrap();

    assert_eq!(
        stored_ids(&fixture.store, &project),
        std::slice::from_ref(&kept)
    );
    assert_eq!(stored_ids(&fixture.store, &other_project), [unrelated]);
    assert!(
        fixture
            .store
            .load_updates(session.clone())
            .unwrap()
            .is_empty()
    );
    assert_eq!(closed(&fixture.backend), std::slice::from_ref(&session));
    assert_eq!(fixture.app.current_session(), None);
    assert!(fixture.app.session_view().is_none());
    let tree = fixture.app.tree();
    let node = tree
        .projects
        .iter()
        .find(|node| node.id == project)
        .unwrap();
    assert_eq!(titles(&node.sessions), ["untitled"]);
    assert!(matches!(
        fixture.app.select_session(session.clone()),
        Err(AppError::UnknownSession(id)) if id == session
    ));
    let reloaded = fixture_with(fixture.store.clone());
    assert!(reloaded.app.session(&session).is_err());
    assert!(reloaded.app.session(&kept).is_ok());
}

#[test]
fn deleting_another_session_keeps_the_current_one_selected() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let (project, deleted) = done_session(&mut fixture, &dir);
    let current = created_session(&mut fixture, &project, Scope::Project, Vec::new());

    fixture.app.delete(deleted, &[]).unwrap();

    assert_eq!(fixture.app.current_session(), Some(current));
    assert!(fixture.app.session_view().is_some());
}

#[test]
fn an_archived_session_can_be_deleted() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let (project, session) = done_session(&mut fixture, &dir);
    fixture.app.archive(session.clone()).unwrap();

    fixture.app.delete(session.clone(), &[]).unwrap();

    assert!(stored_ids(&fixture.store, &project).is_empty());
    assert!(fixture.app.tree().projects[0].archived.is_empty());
    assert!(fixture.store.load_updates(session).unwrap().is_empty());
}

#[test]
fn deleting_an_unknown_session_fails() {
    let mut fixture = fixture();
    let unknown = SessionId::new();

    assert!(matches!(
        fixture.app.delete(unknown.clone(), &[]),
        Err(AppError::UnknownSession(id)) if id == unknown
    ));
    assert!(closed(&fixture.backend).is_empty());
}

#[test]
fn a_failed_save_deletes_nothing() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let (project, session) = done_session(&mut fixture, &dir);
    let updates = fixture.store.load_updates(session.clone()).unwrap();
    let calls = fixture.backend.calls().len();

    fixture.store.fail_next_session_save();
    let refused = fixture.app.delete(session.clone(), &[]);

    assert!(matches!(
        refused,
        Err(AppError::Store(StoreError::Io { .. }))
    ));
    assert_eq!(
        stored_ids(&fixture.store, &project),
        std::slice::from_ref(&session)
    );
    assert_eq!(
        fixture.store.load_updates(session.clone()).unwrap(),
        updates
    );
    assert_eq!(fixture.backend.calls().len(), calls);
    assert_eq!(fixture.app.current_session(), Some(session.clone()));
    assert!(!entries(&fixture.app).is_empty());
    fixture.app.submit_prompt("again", Vec::new()).unwrap();
    send(
        &mut fixture,
        &session,
        Update::AgentText("still here".into()),
    );
}

#[test]
fn a_failed_close_still_deletes_the_transcript_and_is_reported() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let (project, session) = done_session(&mut fixture, &dir);

    fixture.backend.fail_next_call();
    let result = fixture.app.delete(session.clone(), &[]);

    assert!(matches!(
        result,
        Err(AppError::Backend(BackendError::Unavailable(_)))
    ));
    assert!(stored_ids(&fixture.store, &project).is_empty());
    assert!(
        fixture
            .store
            .load_updates(session.clone())
            .unwrap()
            .is_empty()
    );
    assert!(fixture.app.session(&session).is_err());
}

#[test]
fn a_failed_transcript_delete_is_reported_after_closing() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let (project, session) = done_session(&mut fixture, &dir);

    fixture.store.fail_next_delete();
    let result = fixture.app.delete(session.clone(), &[]);

    assert!(matches!(
        result,
        Err(AppError::Store(StoreError::Io { .. }))
    ));
    assert_eq!(closed(&fixture.backend), std::slice::from_ref(&session));
    assert!(stored_ids(&fixture.store, &project).is_empty());
    assert!(fixture.app.session(&session).is_err());
}

#[test]
fn the_close_error_is_reported_before_the_delete_error() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let (_, session) = done_session(&mut fixture, &dir);

    fixture.backend.fail_next_call();
    fixture.store.fail_next_delete();
    let result = fixture.app.delete(session, &[]);

    assert!(matches!(
        result,
        Err(AppError::Backend(BackendError::Unavailable(_)))
    ));
}

#[test]
fn a_running_session_is_closed_and_its_late_events_are_ignored() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    send(
        &mut fixture,
        &session,
        permission(1, &[("allow", PermissionKind::AllowOnce)]),
    );

    fixture.app.delete(session.clone(), &[]).unwrap();

    assert_eq!(
        last_call(&fixture.backend),
        Call::CloseSession {
            session: session.clone()
        }
    );
    let calls = fixture.backend.calls().len();
    let late = [
        BackendEvent::Update {
            session: session.clone(),
            update: Update::AgentText("late".into()),
        },
        BackendEvent::Update {
            session: session.clone(),
            update: Update::TurnEnded(StopReason::Cancelled),
        },
        BackendEvent::SessionClosed {
            session: session.clone(),
            error: "the agent exited".into(),
        },
        BackendEvent::SessionLoaded {
            request: RequestId(1),
            session: session.clone(),
            config: Vec::new(),
        },
        BackendEvent::RequestFailed {
            request: RequestId(1),
            session: session.clone(),
            error: "gone".into(),
        },
    ];
    for event in late {
        fixture.app.apply_event(event).unwrap();
    }
    assert_eq!(fixture.backend.calls().len(), calls);
    assert!(
        fixture
            .store
            .load_updates(session.clone())
            .unwrap()
            .is_empty()
    );
    assert!(fixture.app.session(&session).is_err());
}

#[test]
fn a_session_deleted_while_being_created_ignores_its_creation() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = fixture.app.new_session(project, Scope::Project).unwrap();
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    let (request, _, _) = new_session_request(&fixture.backend);

    fixture.app.delete(session.clone(), &[]).unwrap();
    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: session.clone(),
            backend_session: BackendSessionId::new("b-1"),
            config: Vec::new(),
        })
        .unwrap();

    assert_eq!(
        last_call(&fixture.backend),
        Call::CloseSession {
            session: session.clone()
        }
    );
    assert!(fixture.app.pending.is_empty());
    assert!(fixture.app.queued.is_empty());
    assert!(!fixture.app.attached.contains(&session));
}

#[test]
fn deleting_a_session_leaves_what_others_wait_for() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let waiting = created_session(&mut fixture, &project, Scope::Project, vec![model_option()]);
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    send(
        &mut fixture,
        &waiting,
        permission(1, &[("allow", PermissionKind::AllowOnce)]),
    );
    let creating = fixture
        .app
        .new_session(project.clone(), Scope::Project)
        .unwrap();
    let (request, _, _) = new_session_request(&fixture.backend);
    let (_, deleted) = done_session(&mut fixture, &dir);
    send(
        &mut fixture,
        &deleted,
        permission(2, &[("allow", PermissionKind::AllowOnce)]),
    );

    fixture.app.delete(deleted.clone(), &[]).unwrap();

    assert_eq!(
        fixture.app.waiting_permissions(&waiting).len(),
        1,
        "the other session's request still waits"
    );
    assert!(fixture.app.waiting_permissions(&deleted).is_empty());
    assert!(fixture.app.config.contains_key(&waiting));
    assert!(!fixture.app.config.contains_key(&deleted));
    assert!(!fixture.app.transcripts.contains_key(&deleted));
    assert!(!fixture.app.turn_origins.contains_key(&deleted));
    assert!(fixture.app.turn_origins.contains_key(&waiting));
    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: creating.clone(),
            backend_session: BackendSessionId::new("b-2"),
            config: Vec::new(),
        })
        .unwrap();
    assert!(fixture.app.attached.contains(&creating));
}

#[test]
fn the_delete_summary_tells_about_live_and_archived_sessions() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let (_, session) = done_session(&mut fixture, &dir);
    let live = fixture.app.delete_summary(&session).unwrap();
    assert_eq!(live, fixture.app.archive_summary(&session).unwrap());
    fixture.app.archive(session.clone()).unwrap();

    let archived = fixture.app.delete_summary(&session).unwrap();

    assert_eq!(archived, live);
    let unknown = SessionId::new();
    assert!(matches!(
        fixture.app.delete_summary(&unknown),
        Err(AppError::UnknownSession(id)) if id == unknown
    ));
}

#[test]
fn the_delete_summary_tells_when_a_running_session_is_stopped() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture.app.submit_prompt("go", Vec::new()).unwrap();
    let session = fixture.app.current_session().unwrap();

    let summary = fixture.app.delete_summary(&session).unwrap();

    assert!(summary.stops);
    assert_eq!(summary.status, Status::Running);
}

/// A layer that sets only `link_opener` to `value`.
fn opener_layer(value: &str) -> SettingsLayer {
    let mut layer = SettingsLayer::default();
    layer.set(Level::Project, "link_opener", value).unwrap();
    layer
}

#[test]
fn a_session_opens_links_with_the_opener_of_its_scope() {
    let dir = project_dir(&["api", "web"]);
    let store = MemoryStore::new();
    let (backends, default) = single_claude();
    let mut app = App::load(
        Box::new(RecordingBackend::new()),
        Box::new(store.clone()),
        Box::new(MemoryGit::new()),
        backends,
        default,
        GlobalSettings {
            layer: opener_layer("global-open"),
            home: None,
        },
    )
    .unwrap();
    let project = app.open_project(dir.path()).unwrap();
    let api = RepositoryId::try_from("api").unwrap();
    let web = RepositoryId::try_from("web").unwrap();
    let in_project = app.new_session(project.clone(), Scope::Project).unwrap();
    let in_api = app
        .new_session(project.clone(), Scope::Repository(api.clone()))
        .unwrap();
    let in_web = app
        .new_session(project.clone(), Scope::Repository(web))
        .unwrap();
    let opener = |app: &App, session| app.link_opener(session).unwrap().to_string();

    assert_eq!(opener(&app, &in_project), "global-open");
    assert_eq!(opener(&app, &in_api), "global-open");

    let mut settings = ProjectSettings {
        project: opener_layer("project-open"),
        ..ProjectSettings::default()
    };
    settings.repositories.insert(
        api,
        RepositorySettings {
            layer: opener_layer("api-open --new"),
            ..RepositorySettings::default()
        },
    );
    store.set_settings(project, settings);

    assert_eq!(opener(&app, &in_project), "project-open");
    assert_eq!(opener(&app, &in_api), "api-open --new");
    assert_eq!(opener(&app, &in_web), "project-open");
}

#[test]
fn a_link_opener_needs_a_known_session_and_readable_settings() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = fixture.app.new_session(project, Scope::Project).unwrap();

    assert_eq!(
        fixture.app.link_opener(&session).unwrap(),
        LinkOpener::default()
    );
    fixture.store.fail_next_settings_load();
    assert!(matches!(
        fixture.app.link_opener(&session),
        Err(AppError::Store(_))
    ));
    let unknown = SessionId::new();
    assert!(matches!(
        fixture.app.link_opener(&unknown),
        Err(AppError::UnknownSession(id)) if id == unknown
    ));
}

fn reloaded(fixture: &Fixture) -> App {
    fixture_with(fixture.store.clone()).app
}

#[test]
fn toggling_a_project_collapses_it_and_is_saved() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();

    fixture.app.toggle_project(project.clone()).unwrap();
    assert_eq!(reloaded(&fixture).tree().projects[0].fold, Fold::Folded);

    fixture.app.toggle_project(project).unwrap();
    assert_eq!(reloaded(&fixture).tree().projects[0].fold, Fold::Open);
}

#[test]
fn toggling_an_unknown_project_fails() {
    let mut fixture = fixture();
    let unknown = ProjectId::new();
    assert!(matches!(
        fixture.app.toggle_project(unknown),
        Err(AppError::UnknownProject(_))
    ));
}

#[test]
fn toggling_children_expands_an_orchestrator_and_is_saved() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    fixture
        .app
        .change_session(session.clone(), |session| session.last_activity = 7)
        .unwrap();
    let saves = fixture.store.session_saves(&project);

    fixture.app.toggle_children(session.clone()).unwrap();
    assert!(reloaded(&fixture).tree().projects[0].sessions[0].expanded);
    assert_eq!(fixture.store.session_saves(&project), saves + 1);
    assert_eq!(fixture.app.session(&session).unwrap().last_activity, 7);

    fixture.app.toggle_children(session).unwrap();
    assert!(!reloaded(&fixture).tree().projects[0].sessions[0].expanded);
}

#[test]
fn toggling_the_children_of_an_unknown_session_fails() {
    let mut fixture = fixture();
    assert!(matches!(
        fixture.app.toggle_children(SessionId::new()),
        Err(AppError::UnknownSession(_))
    ));
}

#[test]
fn renaming_fixes_the_title() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());

    fixture
        .app
        .rename_session(session.clone(), "  Short name ")
        .unwrap();
    send(&mut fixture, &session, Update::Title("Agent title".into()));

    let renamed = fixture.app.session(&session).unwrap();
    assert_eq!(renamed.title, "Short name");
    assert_eq!(renamed.title_source, crate::TitleSource::Given);
    let stored = fixture.store.load_sessions(project).unwrap();
    assert_eq!(stored[0].title, "Short name");
}

#[test]
fn a_blank_rename_keeps_the_title() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = created_session(&mut fixture, &project, Scope::Project, Vec::new());
    let before = fixture.app.session(&session).unwrap().title.clone();
    let saves = fixture.store.session_saves(&project);

    fixture.app.rename_session(session.clone(), "   ").unwrap();

    let kept = fixture.app.session(&session).unwrap();
    assert_eq!(kept.title, before);
    assert_eq!(kept.title_source, crate::TitleSource::Derived);
    assert_eq!(fixture.store.session_saves(&project), saves);
}

#[test]
fn renaming_an_unknown_session_fails() {
    let mut fixture = fixture();
    assert!(matches!(
        fixture.app.rename_session(SessionId::new(), "x"),
        Err(AppError::UnknownSession(_))
    ));
    assert!(matches!(
        fixture.app.rename_session(SessionId::new(), " "),
        Err(AppError::UnknownSession(_))
    ));
}
